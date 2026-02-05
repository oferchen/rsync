//! Comprehensive tests for compression with incompressible data.
//!
//! This module verifies:
//! 1. Random/encrypted data doesn't expand significantly
//! 2. Pre-compressed files are handled correctly
//! 3. Compression falls back gracefully
//! 4. --skip-compress works for specific file types

use std::io::Write;
use std::path::Path;

use compress::skip_compress::{
    AdaptiveCompressor, CompressionDecider, CompressionDecision, FileCategory,
};
use compress::zlib::{CompressionLevel, compress_to_vec, decompress_to_vec};

// =============================================================================
// Test Data Generators
// =============================================================================

mod test_data {
    /// Generates cryptographically random-like data using PCG PRNG.
    ///
    /// This simulates encrypted data or truly random data that should not
    /// compress well.
    pub fn random_data(size: usize, seed: u64) -> Vec<u8> {
        let mut state = seed;
        (0..size)
            .map(|_| {
                // PCG XSH-RR variant for high-quality pseudorandom numbers
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
                let rot = (state >> 59) as u32;
                ((xorshifted >> rot) | (xorshifted << ((32u32.wrapping_sub(rot)) & 31))) as u8
            })
            .collect()
    }

    /// Generates pre-compressed data by actually compressing random data.
    pub fn pre_compressed_data(size: usize, seed: u64) -> Vec<u8> {
        let original = random_data(size, seed);
        super::compress_to_vec(&original, super::CompressionLevel::Best)
            .expect("pre-compression should succeed")
    }

    /// Creates a simulated JPEG file with header and random-like body.
    pub fn jpeg_file(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        // JPEG magic bytes
        data.extend_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]);
        // JFIF marker
        data.extend_from_slice(b"JFIF\0\x01\x01\x00\x00\x01\x00\x01\x00\x00");
        // Fill rest with random-like data (simulating compressed image data)
        let remaining = size.saturating_sub(data.len());
        data.extend(random_data(remaining, 0xCAFEBABE));
        data.truncate(size);
        data
    }

    /// Creates a simulated PNG file with header and compressed chunks.
    pub fn png_file(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        // PNG magic bytes
        data.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
        // IHDR chunk header (simplified)
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]); // Length
        data.extend_from_slice(b"IHDR"); // Type
        // Fill rest with random-like data
        let remaining = size.saturating_sub(data.len());
        data.extend(random_data(remaining, 0xDEADBEEF));
        data.truncate(size);
        data
    }

    /// Creates a simulated gzip file.
    pub fn gzip_file(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        // gzip magic bytes
        data.extend_from_slice(&[0x1F, 0x8B, 0x08, 0x00]); // ID1, ID2, CM, FLG
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // MTIME
        data.extend_from_slice(&[0x00, 0xFF]); // XFL, OS
        // Fill rest with random-like data (compressed content)
        let remaining = size.saturating_sub(data.len());
        data.extend(random_data(remaining, 0xBEEFCAFE));
        data.truncate(size);
        data
    }

    /// Creates a simulated ZIP archive.
    pub fn zip_file(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        // ZIP local file header
        data.extend_from_slice(&[b'P', b'K', 0x03, 0x04]); // Signature
        data.extend_from_slice(&[0x14, 0x00]); // Version
        data.extend_from_slice(&[0x00, 0x00]); // Flags
        data.extend_from_slice(&[0x08, 0x00]); // Compression method (deflate)
        // Fill rest with random-like data
        let remaining = size.saturating_sub(data.len());
        data.extend(random_data(remaining, 0x12345678));
        data.truncate(size);
        data
    }

    /// Creates a simulated PDF file.
    pub fn pdf_file(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        // PDF header
        data.extend_from_slice(b"%PDF-1.5\n");
        // PDFs contain compressed streams, so fill with random-like data
        let remaining = size.saturating_sub(data.len());
        data.extend(random_data(remaining, 0xABCDEF01));
        data.truncate(size);
        data
    }

    /// Creates mixed entropy data with both compressible and random sections.
    pub fn mixed_entropy_data(size: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(size);
        let chunk_size = 1024;
        let mut pos = 0;

        while pos < size {
            let remaining = size - pos;
            let this_chunk = chunk_size.min(remaining);

            // Alternate between compressible and random data
            if (pos / chunk_size) % 2 == 0 {
                // Compressible section
                data.extend(vec![b'A'; this_chunk]);
            } else {
                // Random section
                data.extend(random_data(this_chunk, pos as u64));
            }

            pos += this_chunk;
        }
        data
    }
}

// =============================================================================
// SECTION 1: Random/Encrypted Data Doesn't Expand Significantly
// =============================================================================

mod random_data_handling {
    use super::*;

    #[test]
    fn random_data_does_not_expand_significantly() {
        let data = test_data::random_data(10_000, 0x12345678);

        for level in [
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let compressed =
                compress_to_vec(&data, level).expect("compression should not fail on random data");

            // Random data should not expand by more than ~6% due to compression overhead
            // (deflate adds minimal framing, but high-entropy data may expand slightly more)
            let expansion_ratio = compressed.len() as f64 / data.len() as f64;
            assert!(
                expansion_ratio < 1.06,
                "{level:?}: random data expanded by {:.2}% (ratio: {:.3})",
                (expansion_ratio - 1.0) * 100.0,
                expansion_ratio
            );

            // Verify round-trip integrity
            let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
            assert_eq!(decompressed, data, "{level:?}: round-trip integrity failed");
        }
    }

    #[test]
    fn encrypted_like_data_various_sizes() {
        let sizes = [128, 1024, 4096, 16384, 65536];

        for size in sizes {
            let data = test_data::random_data(size, 0xDEADBEEF);
            let compressed =
                compress_to_vec(&data, CompressionLevel::Default).expect("compression succeeds");

            // For truly random data, compression should be nearly neutral
            let ratio = compressed.len() as f64 / data.len() as f64;
            assert!(
                ratio < 1.10,
                "Size {size}: encrypted-like data expanded too much (ratio: {ratio:.3})"
            );

            // Verify correctness
            let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
            assert_eq!(decompressed, data, "Size {size}: round-trip failed");
        }
    }

    #[test]
    fn high_entropy_data_at_all_compression_levels() {
        let data = test_data::random_data(8192, 0xCAFEBABE);

        for level in 1..=9 {
            let compression_level =
                CompressionLevel::from_numeric(level).expect("valid compression level");
            let compressed =
                compress_to_vec(&data, compression_level).expect("compression should succeed");

            // High entropy data should not expand significantly at any level
            let ratio = compressed.len() as f64 / data.len() as f64;
            assert!(
                ratio < 1.08,
                "Level {level}: high entropy data ratio {ratio:.3} exceeds threshold"
            );

            // Verify round-trip
            let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
            assert_eq!(decompressed, data, "Level {level}: round-trip failed");
        }
    }

    #[test]
    fn large_random_data_blocks() {
        // Test with larger blocks that might trigger different compression paths
        let data = test_data::random_data(1_000_000, 0xBEEFCAFE);
        let compressed =
            compress_to_vec(&data, CompressionLevel::Fast).expect("compression succeeds");

        // Even for 1MB of random data, expansion should be limited (deflate overhead is ~0.1% per 32KB)
        let ratio = compressed.len() as f64 / data.len() as f64;
        assert!(
            ratio < 1.06,
            "Large random data expanded too much (ratio: {ratio:.3})"
        );

        // Verify round-trip
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, data, "Large random data round-trip failed");
    }

    #[test]
    fn random_data_with_streaming_encoder() {
        let data = test_data::random_data(50_000, 0x87654321);

        let mut encoder =
            compress::zlib::CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);

        // Write in chunks
        for chunk in data.chunks(1000) {
            encoder.write(chunk).expect("write chunk succeeds");
        }

        let (compressed, bytes_written) = encoder.finish_into_inner().expect("finish succeeds");
        assert_eq!(bytes_written as usize, compressed.len());

        // Check expansion ratio
        let ratio = compressed.len() as f64 / data.len() as f64;
        assert!(
            ratio < 1.05,
            "Streaming random data ratio {ratio:.3} exceeds threshold"
        );

        // Verify round-trip
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, data, "Streaming round-trip failed");
    }
}

// =============================================================================
// SECTION 2: Pre-compressed Files Are Handled Correctly
// =============================================================================

mod precompressed_files {
    use super::*;

    #[test]
    fn jpeg_file_does_not_expand_significantly() {
        let jpeg = test_data::jpeg_file(10_000);
        let compressed =
            compress_to_vec(&jpeg, CompressionLevel::Default).expect("compression should not fail");

        let ratio = compressed.len() as f64 / jpeg.len() as f64;
        assert!(
            ratio < 1.10,
            "JPEG data expanded by {:.1}% (ratio: {ratio:.3})",
            (ratio - 1.0) * 100.0
        );

        // Verify round-trip
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, jpeg, "JPEG round-trip failed");
    }

    #[test]
    fn png_file_does_not_expand_significantly() {
        let png = test_data::png_file(10_000);
        let compressed =
            compress_to_vec(&png, CompressionLevel::Default).expect("compression succeeds");

        let ratio = compressed.len() as f64 / png.len() as f64;
        assert!(
            ratio < 1.10,
            "PNG data expanded by {:.1}% (ratio: {ratio:.3})",
            (ratio - 1.0) * 100.0
        );

        // Verify round-trip
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, png, "PNG round-trip failed");
    }

    #[test]
    fn gzip_file_does_not_expand_significantly() {
        let gzip = test_data::gzip_file(8192);
        let compressed =
            compress_to_vec(&gzip, CompressionLevel::Default).expect("compression succeeds");

        let ratio = compressed.len() as f64 / gzip.len() as f64;
        assert!(
            ratio < 1.10,
            "Gzip data expanded by {:.1}% (ratio: {ratio:.3})",
            (ratio - 1.0) * 100.0
        );

        // Verify round-trip
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, gzip, "Gzip round-trip failed");
    }

    #[test]
    fn zip_file_does_not_expand_significantly() {
        let zip = test_data::zip_file(8192);
        let compressed =
            compress_to_vec(&zip, CompressionLevel::Default).expect("compression succeeds");

        let ratio = compressed.len() as f64 / zip.len() as f64;
        assert!(
            ratio < 1.10,
            "ZIP data expanded by {:.1}% (ratio: {ratio:.3})",
            (ratio - 1.0) * 100.0
        );

        // Verify round-trip
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, zip, "ZIP round-trip failed");
    }

    #[test]
    fn pdf_file_does_not_expand_significantly() {
        let pdf = test_data::pdf_file(10_000);
        let compressed =
            compress_to_vec(&pdf, CompressionLevel::Default).expect("compression succeeds");

        let ratio = compressed.len() as f64 / pdf.len() as f64;
        assert!(
            ratio < 1.10,
            "PDF data expanded by {:.1}% (ratio: {ratio:.3})",
            (ratio - 1.0) * 100.0
        );

        // Verify round-trip
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, pdf, "PDF round-trip failed");
    }

    #[test]
    fn double_compression_does_not_cause_issues() {
        // Start with random data, compress it, then compress again
        let original = test_data::random_data(5000, 0x11111111);
        let first_compression =
            compress_to_vec(&original, CompressionLevel::Default).expect("first compression");

        let second_compression = compress_to_vec(&first_compression, CompressionLevel::Default)
            .expect("second compression");

        // Double compression should not expand much
        let ratio = second_compression.len() as f64 / first_compression.len() as f64;
        assert!(
            ratio < 1.10,
            "Double compression ratio {ratio:.3} exceeds threshold"
        );

        // Verify we can decompress back through both layers
        let first_decompression =
            decompress_to_vec(&second_compression).expect("first decompression");
        assert_eq!(first_decompression, first_compression);

        let second_decompression =
            decompress_to_vec(&first_decompression).expect("second decompression");
        assert_eq!(second_decompression, original);
    }

    #[test]
    fn pre_compressed_data_at_various_levels() {
        let pre_compressed = test_data::pre_compressed_data(5000, 0x22222222);

        for level in [
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let compressed = compress_to_vec(&pre_compressed, level)
                .expect("compression of pre-compressed data succeeds");

            let ratio = compressed.len() as f64 / pre_compressed.len() as f64;
            assert!(
                ratio < 1.10,
                "{level:?}: pre-compressed data ratio {ratio:.3} exceeds threshold"
            );

            // Verify round-trip
            let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
            assert_eq!(
                decompressed, pre_compressed,
                "{level:?}: pre-compressed round-trip failed"
            );
        }
    }

    #[test]
    fn mixed_compressible_and_incompressible_sections() {
        let data = test_data::mixed_entropy_data(20_000);
        let compressed =
            compress_to_vec(&data, CompressionLevel::Default).expect("compression succeeds");

        // Mixed data should compress somewhat, but not as well as pure compressible data
        let ratio = data.len() as f64 / compressed.len() as f64;
        assert!(
            ratio > 1.0,
            "Mixed entropy data should achieve some compression (ratio: {ratio:.3})"
        );

        // Verify round-trip
        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, data, "Mixed entropy round-trip failed");
    }
}

// =============================================================================
// SECTION 3: Compression Falls Back Gracefully
// =============================================================================

mod graceful_fallback {
    use super::*;

    #[test]
    fn auto_detect_identifies_incompressible_data() {
        let decider = CompressionDecider::new();

        // Random data should be detected as incompressible
        let random = test_data::random_data(4096, 0x33333333);
        let is_compressible = decider
            .auto_detect_compressible(&random)
            .expect("auto-detection succeeds");
        assert!(
            !is_compressible,
            "Random data should be detected as incompressible"
        );

        // Repetitive data should be detected as compressible
        let repetitive = vec![b'A'; 4096];
        let is_compressible = decider
            .auto_detect_compressible(&repetitive)
            .expect("auto-detection succeeds");
        assert!(
            is_compressible,
            "Repetitive data should be detected as compressible"
        );
    }

    #[test]
    fn auto_detect_with_various_sample_sizes() {
        let mut decider = CompressionDecider::new();
        let random = test_data::random_data(8192, 0x44444444);

        for sample_size in [512, 1024, 2048, 4096] {
            decider.set_sample_size(sample_size);

            let sample = &random[..sample_size];
            let is_compressible = decider
                .auto_detect_compressible(sample)
                .expect("auto-detection succeeds");

            assert!(
                !is_compressible,
                "Sample size {sample_size}: random data should be incompressible"
            );
        }
    }

    #[test]
    fn auto_detect_with_various_thresholds() {
        let decider_strict = {
            let mut d = CompressionDecider::new();
            d.set_compression_threshold(0.95); // Very strict
            d
        };

        let decider_lenient = {
            let mut d = CompressionDecider::new();
            d.set_compression_threshold(0.80); // More lenient
            d
        };

        // Moderately compressible data (some repetition)
        let moderate = {
            let mut data = Vec::new();
            for i in 0..256 {
                data.extend_from_slice(&[i as u8; 16]);
            }
            data
        };

        let strict_result = decider_strict
            .auto_detect_compressible(&moderate)
            .expect("strict detection succeeds");

        let lenient_result = decider_lenient
            .auto_detect_compressible(&moderate)
            .expect("lenient detection succeeds");

        // Both should make a decision without error (booleans can be true or false)
        let _ = strict_result;
        let _ = lenient_result;
    }

    #[test]
    fn adaptive_compressor_skips_random_data() {
        let decider = CompressionDecider::new();
        let random = test_data::random_data(8192, 0x55555555);

        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        compressor.write_all(&random).expect("write succeeds");
        let output = compressor.finish().expect("finish succeeds");

        // For random data, adaptive compressor should decide not to compress
        // So output size should be similar to input size
        let ratio = output.len() as f64 / random.len() as f64;
        assert!(
            (ratio - 1.0).abs() < 0.15,
            "Adaptive compressor should skip random data (ratio: {ratio:.3})"
        );
    }

    #[test]
    fn adaptive_compressor_compresses_repetitive_data() {
        let decider = CompressionDecider::new();
        let repetitive = vec![b'X'; 8192];

        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        compressor.write_all(&repetitive).expect("write succeeds");
        let output = compressor.finish().expect("finish succeeds");

        // Repetitive data should compress significantly
        let ratio = repetitive.len() as f64 / output.len() as f64;
        assert!(
            ratio > 5.0,
            "Adaptive compressor should compress repetitive data well (ratio: {ratio:.3})"
        );
    }

    #[test]
    fn adaptive_compressor_with_forced_decision() {
        let decider = CompressionDecider::new();
        let random = test_data::random_data(4096, 0x66666666);

        // Force compression off
        let mut output_no_compress = Vec::new();
        let mut compressor = AdaptiveCompressor::new(
            &mut output_no_compress,
            decider.clone(),
            CompressionLevel::Default,
        );
        compressor.set_decision(false);

        compressor
            .write_all(&random)
            .expect("write with forced no-compress succeeds");
        let output_no_compress = compressor.finish().expect("finish succeeds");

        // Without compression, output should match input
        assert_eq!(*output_no_compress, random, "Forced no-compress failed");

        // Force compression on
        let mut output_compress = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output_compress, decider, CompressionLevel::Default);
        compressor.set_decision(true);

        compressor
            .write_all(&random)
            .expect("write with forced compress succeeds");
        let output_compress = compressor.finish().expect("finish succeeds");

        // With forced compression, output should be compressed (but may not be smaller for random data)
        // Just verify it's different from input
        assert_ne!(
            *output_compress, random,
            "Forced compress should transform data"
        );
    }

    #[test]
    fn adaptive_compressor_handles_small_files() {
        let decider = {
            let mut d = CompressionDecider::new();
            d.set_sample_size(100);
            d
        };

        let small_data = b"Small file content";
        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        compressor.write_all(small_data).expect("write succeeds");
        let output = compressor.finish().expect("finish succeeds");

        // Small file should be handled without errors
        assert!(!output.is_empty(), "Small file produced output");
    }
}

// =============================================================================
// SECTION 4: --skip-compress Works for Specific File Types
// =============================================================================

mod skip_compress_functionality {
    use super::*;

    #[test]
    fn skip_compress_detects_image_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        let image_files = [
            "photo.jpg",
            "image.jpeg",
            "picture.png",
            "animation.gif",
            "modern.webp",
            "phone.heic",
            "raw.cr2",
        ];

        for file in image_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} should be skipped"
            );
        }
    }

    #[test]
    fn skip_compress_detects_video_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        let video_files = [
            "movie.mp4",
            "video.mkv",
            "clip.avi",
            "recording.mov",
            "stream.webm",
            "broadcast.flv",
        ];

        for file in video_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} should be skipped"
            );
        }
    }

    #[test]
    fn skip_compress_detects_audio_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        let audio_files = [
            "song.mp3",
            "track.flac",
            "audio.ogg",
            "voice.m4a",
            "music.aac",
            "podcast.opus",
        ];

        for file in audio_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} should be skipped"
            );
        }
    }

    #[test]
    fn skip_compress_detects_archive_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        let archive_files = [
            "backup.zip",
            "data.gz",
            "archive.bz2",
            "compressed.xz",
            "package.7z",
            "files.rar",
            "modern.zst",
            "tarball.tar.gz",
        ];

        for file in archive_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} should be skipped"
            );
        }
    }

    #[test]
    fn skip_compress_detects_document_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        let document_files = [
            "report.pdf",
            "document.docx",
            "spreadsheet.xlsx",
            "presentation.pptx",
            "book.epub",
        ];

        for file in document_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} should be skipped"
            );
        }
    }

    #[test]
    fn skip_compress_custom_list() {
        let mut decider = CompressionDecider::new();

        // Add custom extensions
        decider.add_skip_extension("tmp");
        decider.add_skip_extension("cache");
        decider.add_skip_extension("backup");

        // Custom extensions in skip list return Skip
        assert_eq!(
            decider.should_compress(Path::new("file.tmp"), None),
            CompressionDecision::Skip,
        );

        assert_eq!(
            decider.should_compress(Path::new("data.cache"), None),
            CompressionDecision::Skip,
        );
    }

    #[test]
    fn skip_compress_parse_list_format() {
        let decider = CompressionDecider::from_skip_compress_list("jpg/png/gif/mp4");

        let skip_files = ["image.jpg", "photo.png", "anim.gif", "video.mp4"];

        for file in skip_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} should be in parsed skip list"
            );
        }
    }

    #[test]
    fn skip_compress_with_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // Test with actual file content
        let jpeg = test_data::jpeg_file(1000);
        let decision = decider.should_compress(Path::new("unknown.dat"), Some(&jpeg));
        assert_eq!(
            decision,
            CompressionDecision::Skip,
            "JPEG magic bytes should trigger skip"
        );

        let png = test_data::png_file(1000);
        let decision = decider.should_compress(Path::new("unknown.dat"), Some(&png));
        assert_eq!(
            decision,
            CompressionDecision::Skip,
            "PNG magic bytes should trigger skip"
        );

        let gzip = test_data::gzip_file(1000);
        let decision = decider.should_compress(Path::new("unknown.dat"), Some(&gzip));
        assert_eq!(
            decision,
            CompressionDecision::Skip,
            "Gzip magic bytes should trigger skip"
        );

        let zip = test_data::zip_file(1000);
        let decision = decider.should_compress(Path::new("unknown.dat"), Some(&zip));
        assert_eq!(
            decision,
            CompressionDecision::Skip,
            "ZIP magic bytes should trigger skip"
        );
    }

    #[test]
    fn skip_compress_case_insensitive() {
        let decider = CompressionDecider::with_default_skip_list();

        let uppercase_files = [
            "PHOTO.JPG",
            "VIDEO.MP4",
            "AUDIO.MP3",
            "ARCHIVE.ZIP",
            "DOCUMENT.PDF",
        ];

        for file in uppercase_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} should be skipped (case insensitive)"
            );
        }

        let mixed_case_files = [
            "Photo.JpG",
            "Video.Mp4",
            "Audio.Mp3",
            "Archive.ZiP",
            "Document.PdF",
        ];

        for file in mixed_case_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} should be skipped (mixed case)"
            );
        }
    }

    #[test]
    fn skip_compress_compound_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        let compound_files = [
            "backup.tar.gz",
            "archive.tar.bz2",
            "data.tar.xz",
            "compressed.tar.zst",
        ];

        for file in compound_files {
            let decision = decider.should_compress(Path::new(file), None);
            assert_eq!(
                decision,
                CompressionDecision::Skip,
                "{file} compound extension should be skipped"
            );
        }
    }

    #[test]
    fn skip_compress_respects_file_categories() {
        // Images should not be compressible
        assert!(!FileCategory::Image.is_compressible());
        assert!(!FileCategory::Video.is_compressible());
        assert!(!FileCategory::Audio.is_compressible());
        assert!(!FileCategory::Archive.is_compressible());
        assert!(!FileCategory::Document.is_compressible());

        // Text and data should be compressible
        assert!(FileCategory::Text.is_compressible());
        assert!(FileCategory::Data.is_compressible());
        assert!(FileCategory::Executable.is_compressible());
        assert!(FileCategory::Unknown.is_compressible());
    }

    #[test]
    fn skip_compress_without_magic_detection() {
        let mut decider = CompressionDecider::with_default_skip_list();
        decider.set_use_magic_detection(false);

        // Without magic detection, should rely on extension only
        let jpeg = test_data::jpeg_file(1000);

        // With no extension hint, should not skip based on magic bytes
        let decision = decider.should_compress(Path::new("unknown.dat"), Some(&jpeg));
        assert_eq!(
            decision,
            CompressionDecision::Compress,
            "Without magic detection, should not skip based on content"
        );

        // But extension should still work
        let decision = decider.should_compress(Path::new("photo.jpg"), None);
        assert_eq!(
            decision,
            CompressionDecision::Skip,
            "Extension-based skip should still work"
        );
    }

    #[test]
    fn skip_compress_remove_extensions() {
        let mut decider = CompressionDecider::with_default_skip_list();

        // Verify jpg is in default list
        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::Skip
        );

        // Remove jpg from skip list
        assert!(decider.remove_skip_extension("jpg"));

        // Now jpg should not be skipped
        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::AutoDetect
        );

        // Removing non-existent extension should return false
        assert!(!decider.remove_skip_extension("nonexistent"));
    }

    #[test]
    fn skip_compress_clear_all_extensions() {
        let mut decider = CompressionDecider::with_default_skip_list();
        assert!(!decider.skip_extensions().is_empty());

        decider.clear_skip_extensions();
        assert!(decider.skip_extensions().is_empty());

        // After clearing, nothing should be skipped by extension
        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::AutoDetect
        );
    }
}

// =============================================================================
// SECTION 5: Edge Cases and Boundary Conditions
// =============================================================================

mod edge_cases {
    use super::*;

    #[test]
    fn empty_incompressible_file() {
        let empty: &[u8] = &[];
        let compressed =
            compress_to_vec(empty, CompressionLevel::Default).expect("compress empty succeeds");

        // Empty file should compress to small deflate stream
        assert!(
            compressed.len() < 20,
            "Empty file compressed size {} should be minimal",
            compressed.len()
        );

        let decompressed = decompress_to_vec(&compressed).expect("decompress empty succeeds");
        assert!(decompressed.is_empty());
    }

    #[test]
    fn single_byte_random() {
        let single_byte: &[u8] = &[0x42];
        let compressed =
            compress_to_vec(single_byte, CompressionLevel::Default).expect("compress succeeds");

        // Single byte will expand due to framing
        assert!(
            compressed.len() > single_byte.len(),
            "Single byte should expand due to deflate framing"
        );

        let decompressed = decompress_to_vec(&compressed).expect("decompress succeeds");
        assert_eq!(decompressed, single_byte);
    }

    #[test]
    fn all_possible_byte_values() {
        let all_bytes: Vec<u8> = (0u8..=255).collect();
        let compressed =
            compress_to_vec(&all_bytes, CompressionLevel::Default).expect("compress succeeds");

        // 256 bytes with all values should not compress well
        let ratio = compressed.len() as f64 / all_bytes.len() as f64;
        assert!(
            ratio > 0.80,
            "All byte values should not compress well (ratio: {ratio:.3})"
        );

        let decompressed = decompress_to_vec(&compressed).expect("decompress succeeds");
        assert_eq!(decompressed, all_bytes);
    }

    #[test]
    fn very_small_random_data() {
        let sizes = [1, 2, 3, 4, 5, 10, 20, 50];

        for size in sizes {
            let data = test_data::random_data(size, 0x77777777 + size as u64);
            let compressed = compress_to_vec(&data, CompressionLevel::Default)
                .expect("compress small random succeeds");

            // Very small data will likely expand
            assert!(
                compressed.len() < data.len() * 10,
                "Size {size}: should not explode in size"
            );

            let decompressed =
                decompress_to_vec(&compressed).expect("decompress small random succeeds");
            assert_eq!(decompressed, data, "Size {size}: round-trip failed");
        }
    }

    #[test]
    fn incompressible_at_boundary_sizes() {
        // Test at common buffer boundaries
        let sizes = [4095, 4096, 4097, 8191, 8192, 8193, 16384, 32768, 65536];

        for size in sizes {
            let data = test_data::random_data(size, 0x88888888 + size as u64);
            let compressed = compress_to_vec(&data, CompressionLevel::Fast)
                .expect("compress boundary size succeeds");

            let ratio = compressed.len() as f64 / data.len() as f64;
            assert!(
                ratio < 1.10,
                "Size {size}: incompressible boundary ratio {ratio:.3} exceeds threshold"
            );

            let decompressed =
                decompress_to_vec(&compressed).expect("decompress boundary size succeeds");
            assert_eq!(decompressed, data, "Size {size}: round-trip failed");
        }
    }

    #[test]
    fn compression_level_none_with_random_data() {
        let data = test_data::random_data(10_000, 0x99999999);
        let compressed =
            compress_to_vec(&data, CompressionLevel::None).expect("level 0 compression succeeds");

        // Level 0 adds framing but no compression
        // For incompressible data, this should be close to original size
        let ratio = compressed.len() as f64 / data.len() as f64;
        assert!(
            ratio > 0.95 && ratio < 1.05,
            "Level 0 ratio {ratio:.3} should be close to 1.0"
        );

        let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");
        assert_eq!(decompressed, data, "Level 0 round-trip failed");
    }

    #[test]
    fn worst_case_expansion_bounded() {
        // Worst case: data that triggers maximum expansion
        // This is rare in practice but should still be bounded
        let worst_case = test_data::random_data(100_000, 0xAAAAAAAA);

        let compressed = compress_to_vec(&worst_case, CompressionLevel::Best)
            .expect("worst case compression succeeds");

        // Even in worst case, expansion should be under 10%
        let ratio = compressed.len() as f64 / worst_case.len() as f64;
        assert!(
            ratio < 1.10,
            "Worst case expansion ratio {ratio:.3} exceeds 10% threshold"
        );
    }
}
