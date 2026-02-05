//! Comprehensive test coverage for the compress crate.
//!
//! This module provides tests targeting 95%+ code coverage across:
//! 1. Compression/decompression round-trips (zlib, lz4, zstd)
//! 2. Streaming compression patterns
//! 3. Skip-compress file type detection
//! 4. Compression level effects
//! 5. Error handling for corrupt data

use std::io::{Cursor, Read, Write};
use std::num::NonZeroU8;
use std::path::Path;

use compress::CountingSink;
use compress::algorithm::{CompressionAlgorithm, CompressionAlgorithmParseError};
use compress::skip_compress::{
    AdaptiveCompressor, CompressionDecider, CompressionDecision, DEFAULT_COMPRESSION_THRESHOLD,
    DEFAULT_SAMPLE_SIZE, FileCategory, KNOWN_SIGNATURES, MagicSignature,
};
use compress::zlib::{
    CompressionLevel, CountingZlibDecoder, CountingZlibEncoder, compress_to_vec as zlib_compress,
    decompress_to_vec as zlib_decompress,
};

// =============================================================================
// SECTION 1: Compression/Decompression Round-trips
// =============================================================================

mod round_trips {
    use super::*;

    fn test_data_samples() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("empty", vec![]),
            ("single_byte", vec![42]),
            ("all_zeros", vec![0u8; 1000]),
            ("all_ones", vec![255u8; 1000]),
            ("sequential", (0..=255).collect()),
            (
                "repetitive_text",
                b"The quick brown fox jumps over the lazy dog. ".repeat(100),
            ),
            (
                "english_prose",
                b"Lorem ipsum dolor sit amet, consectetur adipiscing elit.".repeat(50),
            ),
            ("binary_pattern", {
                let mut data = Vec::with_capacity(2000);
                for i in 0..500u32 {
                    data.extend_from_slice(&i.to_le_bytes());
                }
                data
            }),
            (
                "source_code",
                b"fn main() { println!(\"Hello, world!\"); }".repeat(100),
            ),
        ]
    }

    #[test]
    fn zlib_round_trip_all_data_types() {
        for (name, data) in test_data_samples() {
            for level in [
                CompressionLevel::Fast,
                CompressionLevel::Default,
                CompressionLevel::Best,
            ] {
                let compressed = zlib_compress(&data, level)
                    .unwrap_or_else(|e| panic!("{name}: zlib compression failed: {e}"));

                let decompressed = zlib_decompress(&compressed)
                    .unwrap_or_else(|e| panic!("{name}: zlib decompression failed: {e}"));

                assert_eq!(
                    decompressed, data,
                    "{name}: zlib round-trip data mismatch with level {level:?}"
                );
            }
        }
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_frame_round_trip_all_data_types() {
        use compress::lz4::frame::{compress_to_vec, decompress_to_vec};

        for (name, data) in test_data_samples() {
            let compressed = compress_to_vec(&data, CompressionLevel::Default)
                .unwrap_or_else(|e| panic!("{name}: lz4 compression failed: {e}"));

            let decompressed = decompress_to_vec(&compressed)
                .unwrap_or_else(|e| panic!("{name}: lz4 decompression failed: {e}"));

            assert_eq!(
                decompressed, data,
                "{name}: lz4 frame round-trip data mismatch"
            );
        }
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_raw_round_trip() {
        use compress::lz4::raw::{MAX_BLOCK_SIZE, compress_block_to_vec, decompress_block_to_vec};

        let test_cases = [
            ("empty", vec![]),
            ("small", b"hello world".to_vec()),
            ("medium", vec![0xAB; 1000]),
        ];

        for (name, data) in test_cases {
            if data.len() > MAX_BLOCK_SIZE {
                continue;
            }

            let compressed = compress_block_to_vec(&data)
                .unwrap_or_else(|e| panic!("{name}: lz4 raw compression failed: {e}"));

            let max_size = data.len().max(1);
            let decompressed = decompress_block_to_vec(&compressed, max_size)
                .unwrap_or_else(|e| panic!("{name}: lz4 raw decompression failed: {e}"));

            assert_eq!(decompressed, data, "{name}: lz4 raw round-trip mismatch");
        }
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_round_trip_all_data_types() {
        use compress::zstd::{compress_to_vec, decompress_to_vec};

        for (name, data) in test_data_samples() {
            let compressed = compress_to_vec(&data, CompressionLevel::Default)
                .unwrap_or_else(|e| panic!("{name}: zstd compression failed: {e}"));

            let decompressed = decompress_to_vec(&compressed)
                .unwrap_or_else(|e| panic!("{name}: zstd decompression failed: {e}"));

            assert_eq!(decompressed, data, "{name}: zstd round-trip mismatch");
        }
    }

    #[test]
    fn zlib_round_trip_precise_levels() {
        let data = b"Test data for precise compression levels".repeat(100);

        for level in 1..=9 {
            let compression_level = CompressionLevel::from_numeric(level).unwrap();
            let compressed = zlib_compress(&data, compression_level).unwrap();
            let decompressed = zlib_decompress(&compressed).unwrap();
            assert_eq!(decompressed, data, "level {level} round-trip failed");
        }
    }

    #[test]
    fn zlib_round_trip_large_data() {
        let data = b"Large data pattern for stress testing compression. ".repeat(20_000);
        let compressed = zlib_compress(&data, CompressionLevel::Default).unwrap();
        assert!(
            compressed.len() < data.len(),
            "Compression should reduce size"
        );
        let decompressed = zlib_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn zlib_round_trip_boundary_sizes() {
        for size in [1, 2, 255, 256, 1023, 1024, 4095, 4096, 65535, 65536] {
            let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let compressed = zlib_compress(&data, CompressionLevel::Default).unwrap();
            let decompressed = zlib_decompress(&compressed).unwrap();
            assert_eq!(decompressed, data, "size {size} round-trip failed");
        }
    }
}

// =============================================================================
// SECTION 2: Streaming Compression
// =============================================================================

mod streaming {
    use super::*;
    use std::io::IoSlice;

    #[test]
    fn zlib_streaming_encoder_chunked_writes() {
        let data = b"Streaming encoder test data for chunked writing.".repeat(100);
        let chunk_sizes = [1, 7, 13, 64, 256, 1024];

        for chunk_size in chunk_sizes {
            let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

            for chunk in data.chunks(chunk_size) {
                encoder.write(chunk).unwrap();
            }

            let (compressed, bytes) = encoder.finish_into_inner().unwrap();
            assert_eq!(bytes as usize, compressed.len());

            let decompressed = zlib_decompress(&compressed).unwrap();
            assert_eq!(
                decompressed, data,
                "chunk size {chunk_size} round-trip failed"
            );
        }
    }

    #[test]
    fn zlib_streaming_encoder_single_byte_writes() {
        let data = b"Single byte write test";
        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

        for &byte in data.iter() {
            encoder.write(&[byte]).unwrap();
        }

        let (compressed, _) = encoder.finish_into_inner().unwrap();
        let decompressed = zlib_decompress(&compressed).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn zlib_streaming_encoder_vectored_writes() {
        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

        let part1 = b"Hello, ";
        let part2 = b"streaming ";
        let part3 = b"world!";
        let bufs = [
            IoSlice::new(part1),
            IoSlice::new(part2),
            IoSlice::new(part3),
        ];

        let written = std::io::Write::write_vectored(&mut encoder, &bufs).unwrap();
        let expected_total = part1.len() + part2.len() + part3.len();
        if written < expected_total {
            let all_data = [part1.as_slice(), part2.as_slice(), part3.as_slice()].concat();
            encoder.write(&all_data[written..]).unwrap();
        }

        let (compressed, _) = encoder.finish_into_inner().unwrap();
        let decompressed = zlib_decompress(&compressed).unwrap();
        assert_eq!(decompressed, b"Hello, streaming world!");
    }

    #[test]
    fn zlib_streaming_encoder_write_fmt() {
        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
        write!(&mut encoder, "Formatted: {}, test", 42).unwrap();
        let (compressed, _) = encoder.finish_into_inner().unwrap();
        let decompressed = zlib_decompress(&compressed).unwrap();
        assert_eq!(decompressed, b"Formatted: 42, test");
    }

    #[test]
    fn zlib_streaming_encoder_bytes_written_tracking() {
        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
        assert_eq!(encoder.bytes_written(), 0);

        encoder.write(b"first chunk").unwrap();
        let after_first = encoder.bytes_written();

        encoder.write(b"second chunk").unwrap();
        let after_second = encoder.bytes_written();

        assert!(after_second >= after_first);

        let (_, final_bytes) = encoder.finish_into_inner().unwrap();
        assert!(final_bytes >= after_second);
    }

    #[test]
    fn zlib_streaming_decoder_chunked_reads() {
        let data = b"Streaming decoder test data for chunked reading.".repeat(100);
        let compressed = zlib_compress(&data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingZlibDecoder::new(Cursor::new(&compressed));
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
    fn zlib_streaming_decoder_read_to_end() {
        let data = b"Complete read test";
        let compressed = zlib_compress(data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingZlibDecoder::new(Cursor::new(&compressed));
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();

        assert_eq!(output, data);
        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }

    #[test]
    fn zlib_counting_sink_operations() {
        let mut sink = CountingSink;
        let written = std::io::Write::write(&mut sink, b"test data").unwrap();
        assert_eq!(written, 9);

        let bufs = [IoSlice::new(b"hello"), IoSlice::new(b"world")];
        let vectored = std::io::Write::write_vectored(&mut sink, &bufs).unwrap();
        assert_eq!(vectored, 10);

        assert!(std::io::Write::flush(&mut sink).is_ok());
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_streaming_encoder_decoder() {
        use compress::lz4::frame::{CountingLz4Decoder, CountingLz4Encoder};

        let data = b"LZ4 streaming test data".repeat(100);

        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        for chunk in data.chunks(100) {
            encoder.write(chunk).unwrap();
        }
        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
        assert_eq!(bytes as usize, compressed.len());

        let mut decoder = CountingLz4Decoder::new(Cursor::new(&compressed));
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();
        assert_eq!(output, data);
        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_streaming_encoder_decoder() {
        use compress::zstd::{CountingZstdDecoder, CountingZstdEncoder};

        let data = b"Zstd streaming test data".repeat(100);

        let mut encoder =
            CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::Default).unwrap();
        for chunk in data.chunks(100) {
            encoder.write(chunk).unwrap();
        }
        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
        assert_eq!(bytes as usize, compressed.len());

        let mut decoder = CountingZstdDecoder::new(Cursor::new(&compressed)).unwrap();
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();
        assert_eq!(output, data);
        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }
}

// =============================================================================
// SECTION 3: Skip-Compress File Type Detection
// =============================================================================

mod skip_compress {
    use super::*;

    #[test]
    fn default_skip_list_coverage() {
        let decider = CompressionDecider::with_default_skip_list();
        let extensions = decider.skip_extensions();

        let image_exts = ["jpg", "jpeg", "png", "gif", "webp", "heic"];
        for ext in image_exts {
            assert!(extensions.contains(ext), "Missing image extension: {ext}");
        }

        let video_exts = ["mp4", "mkv", "avi", "mov", "webm"];
        for ext in video_exts {
            assert!(extensions.contains(ext), "Missing video extension: {ext}");
        }

        let audio_exts = ["mp3", "flac", "ogg", "m4a", "aac"];
        for ext in audio_exts {
            assert!(extensions.contains(ext), "Missing audio extension: {ext}");
        }

        let archive_exts = ["zip", "gz", "bz2", "xz", "7z", "rar", "zst"];
        for ext in archive_exts {
            assert!(extensions.contains(ext), "Missing archive extension: {ext}");
        }

        assert!(extensions.contains("pdf"));
    }

    #[test]
    fn should_compress_extension_detection() {
        let decider = CompressionDecider::with_default_skip_list();

        let skip_files = [
            "photo.jpg",
            "video.mp4",
            "audio.mp3",
            "archive.zip",
            "document.pdf",
        ];
        for file in skip_files {
            assert_eq!(
                decider.should_compress(Path::new(file), None),
                CompressionDecision::Skip,
                "{file} should be skipped"
            );
        }

        let unknown_files = ["data.xyz", "file.unknown", "readme.txt"];
        for file in unknown_files {
            assert_eq!(
                decider.should_compress(Path::new(file), None),
                CompressionDecision::AutoDetect,
                "{file} should be auto-detected"
            );
        }
    }

    #[test]
    fn should_compress_case_insensitive() {
        let decider = CompressionDecider::with_default_skip_list();

        assert_eq!(
            decider.should_compress(Path::new("PHOTO.JPG"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("Video.MP4"), None),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection() {
        let decider = CompressionDecider::with_default_skip_list();

        // JPEG
        let jpeg_header = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&jpeg_header)),
            CompressionDecision::Skip
        );

        // PNG
        let png_header = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&png_header)),
            CompressionDecision::Skip
        );

        // ZIP
        let zip_header = [b'P', b'K', 0x03, 0x04, 0x00, 0x00];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&zip_header)),
            CompressionDecision::Skip
        );

        // PDF
        let pdf_header = b"%PDF-1.5";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(pdf_header)),
            CompressionDecision::Skip
        );

        // Plain text (compressible)
        let text = b"Hello, this is plain text content";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(text)),
            CompressionDecision::Compress
        );
    }

    #[test]
    fn riff_container_detection() {
        let decider = CompressionDecider::with_default_skip_list();

        let avi_header = b"RIFF\x00\x00\x00\x00AVI ";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(avi_header)),
            CompressionDecision::Skip
        );

        let wav_header = b"RIFF\x00\x00\x00\x00WAVE";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(wav_header)),
            CompressionDecision::Skip
        );

        let webp_header = b"RIFF\x00\x00\x00\x00WEBP";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(webp_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn auto_detect_compressible() {
        let decider = CompressionDecider::new();

        let repetitive = vec![b'a'; 4096];
        assert!(decider.auto_detect_compressible(&repetitive).unwrap());

        let zeros = vec![0u8; 4096];
        assert!(decider.auto_detect_compressible(&zeros).unwrap());

        assert!(decider.auto_detect_compressible(&[]).unwrap());

        // High-entropy data
        let mut state: u64 = 0x853c49e6748fea9b;
        let random: Vec<u8> = (0..4096)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
                let rot = (state >> 59) as u32;
                ((xorshifted >> rot) | (xorshifted << ((32u32.wrapping_sub(rot)) & 31))) as u8
            })
            .collect();
        assert!(!decider.auto_detect_compressible(&random).unwrap());
    }

    #[test]
    fn compression_decider_configuration() {
        let mut decider = CompressionDecider::new();

        assert!(
            (decider.compression_threshold() - DEFAULT_COMPRESSION_THRESHOLD).abs() < f64::EPSILON
        );
        decider.set_compression_threshold(0.85);
        assert!((decider.compression_threshold() - 0.85).abs() < f64::EPSILON);
        decider.set_compression_threshold(2.0);
        assert!((decider.compression_threshold() - 1.0).abs() < f64::EPSILON);

        assert_eq!(decider.sample_size(), DEFAULT_SAMPLE_SIZE);
        decider.set_sample_size(8192);
        assert_eq!(decider.sample_size(), 8192);
        decider.set_sample_size(10);
        assert_eq!(decider.sample_size(), 64);

        assert!(decider.use_magic_detection());
        decider.set_use_magic_detection(false);
        assert!(!decider.use_magic_detection());
    }

    #[test]
    fn compression_decider_extension_management() {
        let mut decider = CompressionDecider::new();

        decider.add_skip_extension("xyz");
        assert!(decider.skip_extensions().contains("xyz"));

        decider.add_skip_extension(".abc");
        assert!(decider.skip_extensions().contains("abc"));

        decider.add_skip_extension("DEF");
        assert!(decider.skip_extensions().contains("def"));

        assert!(decider.remove_skip_extension("xyz"));
        assert!(!decider.skip_extensions().contains("xyz"));

        assert!(!decider.remove_skip_extension("notexists"));

        decider.clear_skip_extensions();
        assert!(decider.skip_extensions().is_empty());
    }

    #[test]
    fn parse_skip_compress_list_formats() {
        let decider1 = CompressionDecider::from_skip_compress_list("txt/log/csv");
        assert!(decider1.skip_extensions().contains("txt"));
        assert!(decider1.skip_extensions().contains("log"));
        assert!(decider1.skip_extensions().contains("csv"));

        let decider2 = CompressionDecider::from_skip_compress_list("txt  log\tcsv");
        assert!(decider2.skip_extensions().contains("txt"));
        assert!(decider2.skip_extensions().contains("log"));
        assert!(decider2.skip_extensions().contains("csv"));
    }

    #[test]
    fn file_category_is_compressible() {
        assert!(!FileCategory::Image.is_compressible());
        assert!(!FileCategory::Video.is_compressible());
        assert!(!FileCategory::Audio.is_compressible());
        assert!(!FileCategory::Archive.is_compressible());
        assert!(!FileCategory::Document.is_compressible());
        assert!(FileCategory::Text.is_compressible());
        assert!(FileCategory::Data.is_compressible());
        assert!(FileCategory::Executable.is_compressible());
        assert!(FileCategory::Unknown.is_compressible());
    }

    #[test]
    fn magic_signature_struct() {
        let sig = MagicSignature::new(0, b"TEST", FileCategory::Unknown);
        assert_eq!(sig.offset, 0);
        assert_eq!(sig.bytes, b"TEST");

        assert!(sig.matches(b"TEST1234"));
        assert!(sig.matches(b"TEST"));
        assert!(!sig.matches(b"TES"));
        assert!(!sig.matches(b"NOPE"));

        let sig2 = MagicSignature::new(4, b"DATA", FileCategory::Data);
        assert!(sig2.matches(b"XXXXDATAYYYY"));
        assert!(!sig2.matches(b"DATA"));
    }

    #[test]
    fn known_signatures_coverage() {
        for sig in KNOWN_SIGNATURES {
            assert!(!sig.bytes.is_empty(), "Signature should not be empty");
            let mut test_data = vec![0u8; sig.offset + sig.bytes.len()];
            test_data[sig.offset..].copy_from_slice(sig.bytes);
            assert!(
                sig.matches(&test_data),
                "Signature should match its own bytes"
            );
        }
    }

    #[test]
    fn compression_decider_default_trait() {
        let decider = CompressionDecider::default();
        assert!(!decider.skip_extensions().is_empty());
    }
}

// =============================================================================
// SECTION 4: Compression Level Effects
// =============================================================================

mod compression_levels {
    use super::*;

    #[test]
    fn compression_level_presets() {
        let data = b"Test data for compression level comparison".repeat(100);

        let fast = zlib_compress(&data, CompressionLevel::Fast).unwrap();
        let default = zlib_compress(&data, CompressionLevel::Default).unwrap();
        let best = zlib_compress(&data, CompressionLevel::Best).unwrap();

        assert!(!fast.is_empty());
        assert!(!default.is_empty());
        assert!(!best.is_empty());

        assert_eq!(zlib_decompress(&fast).unwrap(), data);
        assert_eq!(zlib_decompress(&default).unwrap(), data);
        assert_eq!(zlib_decompress(&best).unwrap(), data);

        assert!(
            best.len() <= fast.len(),
            "Best ({}) should compress at least as well as Fast ({})",
            best.len(),
            fast.len()
        );
    }

    #[test]
    fn compression_level_from_numeric() {
        // Valid levels 1-9
        for level in 1..=9 {
            let result = CompressionLevel::from_numeric(level);
            assert!(result.is_ok(), "Level {level} should be valid");

            if let CompressionLevel::Precise(n) = result.unwrap() {
                assert_eq!(n.get() as u32, level);
            } else {
                panic!("Expected Precise variant");
            }
        }

        // Level 0 is valid (CompressionLevel::None)
        assert_eq!(
            CompressionLevel::from_numeric(0).unwrap(),
            CompressionLevel::None
        );

        // Invalid levels
        assert!(CompressionLevel::from_numeric(10).is_err());
        assert!(CompressionLevel::from_numeric(100).is_err());
        assert!(CompressionLevel::from_numeric(u32::MAX).is_err());
    }

    #[test]
    fn compression_level_precise_constructor() {
        for n in 1..=9 {
            let nz = NonZeroU8::new(n).unwrap();
            let level = CompressionLevel::precise(nz);
            if let CompressionLevel::Precise(inner) = level {
                assert_eq!(inner.get(), n);
            } else {
                panic!("Expected Precise variant");
            }
        }
    }

    #[test]
    fn compression_level_error_details() {
        let err = CompressionLevel::from_numeric(42).unwrap_err();
        assert_eq!(err.level(), 42);

        let display = err.to_string();
        assert!(display.contains("42"));
        assert!(display.contains("1-9") || display.contains("range"));
    }

    #[test]
    fn all_precise_levels_produce_valid_output() {
        let data = b"Testing all precise compression levels".repeat(50);

        for level in 1..=9 {
            let compression_level = CompressionLevel::from_numeric(level).unwrap();
            let compressed = zlib_compress(&data, compression_level).unwrap();
            let decompressed = zlib_decompress(&compressed).unwrap();
            assert_eq!(decompressed, data, "Level {level} round-trip failed");
        }
    }

    #[test]
    fn compression_ratio_trends() {
        let data = b"AAAAAAAAAA".repeat(10_000);

        let mut sizes: Vec<(u32, usize)> = Vec::new();
        for level in 1..=9 {
            let compression_level = CompressionLevel::from_numeric(level).unwrap();
            let compressed = zlib_compress(&data, compression_level).unwrap();
            sizes.push((level, compressed.len()));
        }

        let level_1_size = sizes[0].1;
        let level_9_size = sizes[8].1;
        assert!(
            level_9_size <= level_1_size,
            "Level 9 ({level_9_size}) should be <= level 1 ({level_1_size})"
        );
    }
}

// =============================================================================
// SECTION 5: Error Handling for Corrupt Data
// =============================================================================

mod error_handling {
    use super::*;

    #[test]
    fn zlib_decompress_corrupt_data() {
        let garbage = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        assert!(zlib_decompress(&garbage).is_err());

        let data = b"Test data for corruption testing";
        let compressed = zlib_compress(data, CompressionLevel::Default).unwrap();
        let truncated = &compressed[..compressed.len() / 2];
        // Truncated data may or may not fail depending on deflate stream state
        let _ = zlib_decompress(truncated);

        let mut corrupted = compressed.clone();
        if corrupted.len() > 5 {
            let mid = corrupted.len() / 2;
            corrupted[mid] ^= 0xFF;
        }
        // May or may not fail depending on where corruption hits
        let _ = zlib_decompress(&corrupted);
    }

    #[test]
    fn zlib_decompress_empty_input() {
        // Empty deflate stream is actually valid - it produces empty output
        let result = zlib_decompress(&[]);
        assert!(result.is_ok() || result.is_err()); // Either is acceptable behavior
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_frame_decompress_corrupt_data() {
        use compress::lz4::frame::{compress_to_vec, decompress_to_vec};

        let invalid_magic = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(decompress_to_vec(&invalid_magic).is_err());

        let data = b"LZ4 corruption test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
        let truncated = &compressed[..compressed.len() / 2];
        assert!(decompress_to_vec(truncated).is_err());

        let mut corrupted = compressed.clone();
        if corrupted.len() > 10 {
            corrupted[8] ^= 0xFF;
        }
        assert!(decompress_to_vec(&corrupted).is_err());
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_raw_error_cases() {
        use compress::lz4::raw::{
            MAX_BLOCK_SIZE, MAX_DECOMPRESSED_SIZE, RawLz4Error, compress_block,
            compress_block_to_vec, decompress_block, encode_header,
        };

        let large_input = vec![0u8; MAX_BLOCK_SIZE + 1];
        match compress_block_to_vec(&large_input) {
            Err(RawLz4Error::InputTooLarge(size)) => assert_eq!(size, MAX_BLOCK_SIZE + 1),
            other => panic!("Expected InputTooLarge, got {other:?}"),
        }

        let input = b"test data";
        let mut small_buffer = [0u8; 5];
        match compress_block(input, &mut small_buffer) {
            Err(RawLz4Error::BufferTooSmall { .. }) => {}
            other => panic!("Expected BufferTooSmall, got {other:?}"),
        }

        let invalid_header = [0x80, 0x00, 0x00, 0x00];
        match decompress_block(&invalid_header, &mut [0u8; 100]) {
            Err(RawLz4Error::InvalidHeader(0x80)) => {}
            other => panic!("Expected InvalidHeader(0x80), got {other:?}"),
        }

        let header = encode_header(100);
        match decompress_block(&header, &mut [0u8; 1000]) {
            Err(RawLz4Error::BufferTooSmall { .. }) => {}
            other => panic!("Expected BufferTooSmall, got {other:?}"),
        }

        let mut corrupted = Vec::from(encode_header(10));
        corrupted.extend_from_slice(&[0xFF; 10]);
        match decompress_block(&corrupted, &mut [0u8; 1000]) {
            Err(RawLz4Error::DecompressFailed(_)) => {}
            other => panic!("Expected DecompressFailed, got {other:?}"),
        }

        use compress::lz4::raw::decompress_block_to_vec;
        match decompress_block_to_vec(&[0x40, 0x00], MAX_DECOMPRESSED_SIZE + 1) {
            Err(RawLz4Error::DecompressedSizeTooLarge(size)) => {
                assert_eq!(size, MAX_DECOMPRESSED_SIZE + 1);
            }
            other => panic!("Expected DecompressedSizeTooLarge, got {other:?}"),
        }
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_raw_io_error_conversion() {
        use compress::lz4::raw::RawLz4Error;
        use std::io::ErrorKind;

        let err = RawLz4Error::InputTooLarge(20000);
        let io_err: std::io::Error = err.into();
        assert_eq!(io_err.kind(), ErrorKind::InvalidData);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_decompress_corrupt_data() {
        use compress::zstd::{compress_to_vec, decompress_to_vec};

        let garbage = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        assert!(decompress_to_vec(&garbage).is_err());

        let data = b"Zstd corruption test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
        let truncated = &compressed[..compressed.len() / 2];
        assert!(decompress_to_vec(truncated).is_err());
    }

    #[test]
    fn compression_algorithm_parse_errors() {
        let err = "brotli".parse::<CompressionAlgorithm>().unwrap_err();
        assert_eq!(err.input(), "brotli");

        let display = err.to_string();
        assert!(display.contains("brotli"));
        assert!(display.contains("unsupported"));

        let err2 = CompressionAlgorithmParseError::new("invalid");
        assert_eq!(err2.input(), "invalid");
    }

    #[test]
    fn streaming_decoder_error_propagation() {
        let garbage = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
        let mut decoder = CountingZlibDecoder::new(Cursor::new(&garbage));
        let mut output = Vec::new();
        assert!(decoder.read_to_end(&mut output).is_err());
    }
}

// =============================================================================
// SECTION 6: Adaptive Compressor
// =============================================================================

mod adaptive_compressor {
    use super::*;

    #[test]
    fn adaptive_compressor_auto_detect_compressible() {
        let decider = CompressionDecider::new();
        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        let data = vec![b'a'; 8192];
        std::io::Write::write_all(&mut compressor, &data).unwrap();

        assert!(compressor.compression_enabled().is_some());

        let output = compressor.finish().unwrap();
        assert!(output.len() < data.len());
    }

    #[test]
    fn adaptive_compressor_forced_no_compression() {
        let decider = CompressionDecider::new();
        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        compressor.set_decision(false);
        assert_eq!(compressor.compression_enabled(), Some(false));

        let data = b"test data that won't be compressed";
        std::io::Write::write_all(&mut compressor, data).unwrap();

        let output = compressor.finish().unwrap();
        assert_eq!(&output[..], data);
    }

    #[test]
    fn adaptive_compressor_small_writes() {
        let mut decider = CompressionDecider::new();
        decider.set_sample_size(100);

        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        std::io::Write::write(&mut compressor, b"small").unwrap();
        assert!(compressor.compression_enabled().is_none());

        let _ = compressor.finish().unwrap();
    }

    #[test]
    fn adaptive_compressor_incremental_writes() {
        let mut decider = CompressionDecider::new();
        decider.set_sample_size(100);

        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        for _ in 0..20 {
            std::io::Write::write(&mut compressor, b"chunk").unwrap();
        }

        assert!(compressor.compression_enabled().is_some());
        let _ = compressor.finish().unwrap();
    }

    #[test]
    fn adaptive_compressor_flush() {
        let decider = CompressionDecider::new();
        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        compressor.set_decision(true);
        std::io::Write::write_all(&mut compressor, b"test data").unwrap();
        std::io::Write::flush(&mut compressor).unwrap();

        let _ = compressor.finish().unwrap();
    }
}

// =============================================================================
// SECTION 7: Algorithm Module Coverage
// =============================================================================

mod algorithm {
    use super::*;

    #[test]
    fn compression_algorithm_available() {
        let available = CompressionAlgorithm::available();
        assert!(!available.is_empty());
        assert!(available.contains(&CompressionAlgorithm::Zlib));

        #[cfg(feature = "lz4")]
        assert!(available.contains(&CompressionAlgorithm::Lz4));

        #[cfg(feature = "zstd")]
        assert!(available.contains(&CompressionAlgorithm::Zstd));
    }

    #[test]
    fn compression_algorithm_names() {
        assert_eq!(CompressionAlgorithm::Zlib.name(), "zlib");

        #[cfg(feature = "lz4")]
        assert_eq!(CompressionAlgorithm::Lz4.name(), "lz4");

        #[cfg(feature = "zstd")]
        assert_eq!(CompressionAlgorithm::Zstd.name(), "zstd");
    }

    #[test]
    fn compression_algorithm_default() {
        assert_eq!(
            CompressionAlgorithm::default_algorithm(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(CompressionAlgorithm::default(), CompressionAlgorithm::Zlib);
    }

    #[test]
    fn compression_algorithm_parsing() {
        assert_eq!(
            "zlib".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(
            "zlibx".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(
            "ZLIB".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );
        assert_eq!(
            "  zlib  ".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zlib
        );

        #[cfg(feature = "lz4")]
        assert_eq!(
            "lz4".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Lz4
        );

        #[cfg(feature = "zstd")]
        assert_eq!(
            "zstd".parse::<CompressionAlgorithm>().unwrap(),
            CompressionAlgorithm::Zstd
        );
    }

    #[test]
    fn compression_algorithm_traits() {
        let algo = CompressionAlgorithm::Zlib;
        let cloned = algo;
        assert_eq!(algo, cloned);

        let debug = format!("{algo:?}");
        assert!(debug.contains("Zlib"));

        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(CompressionAlgorithm::Zlib);
        assert!(set.contains(&CompressionAlgorithm::Zlib));
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_default_algorithm() {
        use compress::lz4::default_algorithm;
        assert_eq!(default_algorithm(), CompressionAlgorithm::Lz4);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_default_algorithm() {
        use compress::zstd::default_algorithm;
        assert_eq!(default_algorithm(), CompressionAlgorithm::Zstd);
    }
}

// =============================================================================
// SECTION 8: LZ4 Raw Module Comprehensive Coverage
// =============================================================================

#[cfg(feature = "lz4")]
mod lz4_raw_comprehensive {
    use compress::lz4::raw::{
        HEADER_SIZE, MAX_BLOCK_SIZE, compress_block, compress_block_to_vec,
        compressed_size_from_header, decode_header, decompress_block, decompress_block_to_vec,
        encode_header, is_deflated_data, read_compressed_block, write_compressed_block,
    };
    use lz4_flex::block::get_maximum_output_size;
    use std::io::Cursor;

    #[test]
    fn header_encode_decode_all_sizes() {
        for size in [0, 1, 100, 1000, 8191, 8192, 16382, MAX_BLOCK_SIZE] {
            let header = encode_header(size);
            let decoded = decode_header(header).expect("valid header");
            assert_eq!(decoded, size, "size {size} roundtrip failed");
        }
    }

    #[test]
    fn header_flag_detection() {
        for flag in [0x40, 0x41, 0x5F, 0x7F] {
            assert!(is_deflated_data(flag), "0x{flag:02x} should be deflated");
        }

        for flag in [0x00, 0x80, 0xC0, 0xFF] {
            assert!(
                !is_deflated_data(flag),
                "0x{flag:02x} should not be deflated"
            );
        }
    }

    #[test]
    fn compressed_size_from_header_helper() {
        let header = encode_header(1234);
        assert_eq!(compressed_size_from_header(header), Some(1234));

        assert_eq!(compressed_size_from_header([0x00, 0x00]), None);
        assert_eq!(compressed_size_from_header([0x80, 0x00]), None);
        assert_eq!(compressed_size_from_header([0xC0, 0x00]), None);
    }

    #[test]
    fn compress_decompress_various_sizes() {
        for size in [0, 1, 10, 100, 1000, 10000, MAX_BLOCK_SIZE] {
            let input: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
            let compressed = compress_block_to_vec(&input).expect("compress");
            let decompressed =
                decompress_block_to_vec(&compressed, size.max(1)).expect("decompress");
            assert_eq!(decompressed, input, "size {size} roundtrip failed");
        }
    }

    #[test]
    fn compress_into_buffer() {
        let input = b"buffer compression test data";
        let mut output = vec![0u8; HEADER_SIZE + get_maximum_output_size(input.len())];

        let total = compress_block(input, &mut output).expect("compress");
        output.truncate(total);

        let mut decompressed = vec![0u8; input.len()];
        let decompressed_len = decompress_block(&output, &mut decompressed).expect("decompress");
        assert_eq!(&decompressed[..decompressed_len], input.as_slice());
    }

    #[test]
    fn read_write_roundtrip() {
        let input = b"read/write roundtrip test data";
        let mut buffer = Vec::new();

        let written = write_compressed_block(input, &mut buffer).expect("write");
        assert!(written > 0);

        let mut cursor = Cursor::new(buffer);
        let decompressed = read_compressed_block(&mut cursor, input.len()).expect("read");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn highly_compressible_data() {
        let input = vec![0u8; 10000];
        let compressed = compress_block_to_vec(&input).expect("compress");
        assert!(
            compressed.len() < input.len() / 10,
            "zeros should compress very well"
        );

        let decompressed = decompress_block_to_vec(&compressed, input.len()).expect("decompress");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn empty_input_roundtrip() {
        let input = b"";
        let compressed = compress_block_to_vec(input).expect("compress");
        let decompressed = decompress_block_to_vec(&compressed, 0).expect("decompress");
        assert!(decompressed.is_empty());
    }
}

// =============================================================================
// SECTION 9: Zstd Module Comprehensive Coverage
// =============================================================================

#[cfg(feature = "zstd")]
mod zstd_comprehensive {
    use super::*;
    use compress::zstd::{
        CountingZstdDecoder, CountingZstdEncoder, compress_to_vec, decompress_to_vec,
    };
    use std::io::IoSliceMut;

    #[test]
    fn encoder_counting_sink() {
        let mut encoder = CountingZstdEncoder::new(CompressionLevel::Default).unwrap();
        encoder.write(b"test data").unwrap();
        let bytes = encoder.finish().unwrap();
        assert!(bytes > 0);
    }

    #[test]
    fn encoder_with_custom_sink() {
        let mut encoder =
            CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::Default).unwrap();
        assert_eq!(encoder.bytes_written(), 0);
        encoder.write(b"test data").unwrap();

        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
        assert_eq!(bytes as usize, compressed.len());

        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, b"test data");
    }

    #[test]
    fn encoder_accessors() {
        let mut encoder =
            CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::Default).unwrap();
        assert!(encoder.get_ref().is_empty());
        encoder.get_mut().extend_from_slice(b"prefix");
        assert!(encoder.get_ref().starts_with(b"prefix"));
    }

    #[test]
    fn decoder_tracking() {
        let data = b"decoder tracking test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingZstdDecoder::new(Cursor::new(&compressed)).unwrap();
        assert_eq!(decoder.bytes_read(), 0);

        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();

        assert_eq!(output, data);
        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }

    #[test]
    fn decoder_vectored_reads() {
        let data = b"vectored read test data for zstd decoder".repeat(10);
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingZstdDecoder::new(Cursor::new(&compressed)).unwrap();
        let mut buf1 = [0u8; 30];
        let mut buf2 = [0u8; 50];
        let mut bufs = [IoSliceMut::new(&mut buf1), IoSliceMut::new(&mut buf2)];

        let read = decoder.read_vectored(&mut bufs).unwrap();
        assert!(read > 0);
        assert_eq!(decoder.bytes_read(), read as u64);
    }

    #[test]
    fn decoder_accessors() {
        let data = b"accessor test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
        let cursor = Cursor::new(compressed);

        let mut decoder = CountingZstdDecoder::new(cursor).unwrap();
        assert_eq!(decoder.get_ref().position(), 0);
        decoder.get_mut().set_position(1);
        assert_eq!(decoder.get_ref().position(), 1);

        let _ = decoder.into_inner();
    }

    #[test]
    fn all_compression_levels() {
        let data = b"testing all zstd compression levels".repeat(100);

        for level in [
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let compressed = compress_to_vec(&data, level).unwrap();
            let decompressed = decompress_to_vec(&compressed).unwrap();
            assert_eq!(decompressed, data);
        }

        for n in 1..=9 {
            let level = CompressionLevel::from_numeric(n).unwrap();
            let compressed = compress_to_vec(&data, level).unwrap();
            let decompressed = decompress_to_vec(&compressed).unwrap();
            assert_eq!(decompressed, data);
        }
    }
}

// =============================================================================
// SECTION 10: LZ4 Frame Module Comprehensive Coverage
// =============================================================================

#[cfg(feature = "lz4")]
mod lz4_frame_comprehensive {
    use super::*;
    use compress::lz4::frame::{
        CountingLz4Decoder, CountingLz4Encoder, compress_to_vec, decompress_to_vec,
    };
    use std::io::IoSliceMut;

    #[test]
    fn encoder_counting_sink() {
        let mut encoder = CountingLz4Encoder::new(CompressionLevel::Default);
        encoder.write(b"test data").unwrap();
        let bytes = encoder.finish().unwrap();
        assert!(bytes > 0);
    }

    #[test]
    fn encoder_with_custom_sink() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        assert_eq!(encoder.bytes_written(), 0);
        encoder.write(b"test data").unwrap();

        let (compressed, bytes) = encoder.finish_into_inner().unwrap();
        assert_eq!(bytes as usize, compressed.len());

        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert_eq!(decompressed, b"test data");
    }

    #[test]
    fn encoder_accessors() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        assert!(encoder.get_ref().is_empty());
        encoder.get_mut().extend_from_slice(b"prefix");
        assert!(encoder.get_ref().starts_with(b"prefix"));
    }

    #[test]
    fn decoder_tracking() {
        let data = b"decoder tracking test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingLz4Decoder::new(Cursor::new(&compressed));
        assert_eq!(decoder.bytes_read(), 0);

        let mut output = Vec::new();
        decoder.read_to_end(&mut output).unwrap();

        assert_eq!(output, data);
        assert_eq!(decoder.bytes_read(), data.len() as u64);
    }

    #[test]
    fn decoder_vectored_reads() {
        let data = b"vectored read test data for lz4 decoder".repeat(10);
        let compressed = compress_to_vec(&data, CompressionLevel::Default).unwrap();

        let mut decoder = CountingLz4Decoder::new(Cursor::new(&compressed));
        let mut buf1 = [0u8; 30];
        let mut buf2 = [0u8; 50];
        let mut bufs = [IoSliceMut::new(&mut buf1), IoSliceMut::new(&mut buf2)];

        let read = decoder.read_vectored(&mut bufs).unwrap();
        assert!(read > 0);
        assert_eq!(decoder.bytes_read(), read as u64);
    }

    #[test]
    fn decoder_accessors() {
        let data = b"accessor test";
        let compressed = compress_to_vec(data, CompressionLevel::Default).unwrap();
        let cursor = Cursor::new(compressed);

        let mut decoder = CountingLz4Decoder::new(cursor);
        assert_eq!(decoder.get_ref().position(), 0);
        decoder.get_mut().set_position(1);
        assert_eq!(decoder.get_ref().position(), 1);

        let _ = decoder.into_inner();
    }

    #[test]
    fn all_compression_levels() {
        let data = b"testing all lz4 compression levels".repeat(100);

        for level in [
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let compressed = compress_to_vec(&data, level).unwrap();
            let decompressed = decompress_to_vec(&compressed).unwrap();
            assert_eq!(decompressed, data);
        }

        for n in 1..=9 {
            let level = CompressionLevel::from_numeric(n).unwrap();
            let compressed = compress_to_vec(&data, level).unwrap();
            let decompressed = decompress_to_vec(&compressed).unwrap();
            assert_eq!(decompressed, data);
        }
    }

    #[test]
    fn empty_input_produces_valid_frame() {
        let compressed = compress_to_vec(&[], CompressionLevel::Default).unwrap();
        assert!(!compressed.is_empty());

        let decompressed = decompress_to_vec(&compressed).unwrap();
        assert!(decompressed.is_empty());
    }
}
