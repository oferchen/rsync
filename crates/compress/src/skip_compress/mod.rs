//! Compression tuning based on file type.
//!
//! This module implements intelligent compression decisions to avoid wasting CPU
//! on files that are already compressed or otherwise incompressible.
//!
//! # Design
//!
//! The compression decider uses a three-pronged approach:
//!
//! 1. **Extension-based detection**: Fast O(1) lookup for known file extensions
//! 2. **Magic byte detection**: Identify compressed files by their headers
//! 3. **Auto-detection**: Sample-based compression ratio analysis
//!
//! # Upstream Compatibility
//!
//! This module mirrors upstream rsync's `--skip-compress` functionality, which
//! allows users to specify file extensions that should not be compressed during
//! transfer. The default skip list includes common compressed formats.
//!
//! # Example
//!
//! ```
//! use compress::skip_compress::{CompressionDecider, CompressionDecision};
//! use std::path::Path;
//!
//! let decider = CompressionDecider::with_default_skip_list();
//!
//! // JPEG files are known to be incompressible
//! assert_eq!(
//!     decider.should_compress(Path::new("photo.jpg"), None),
//!     CompressionDecision::Skip
//! );
//!
//! // Unknown extensions without content return AutoDetect
//! assert_eq!(
//!     decider.should_compress(Path::new("document.txt"), None),
//!     CompressionDecision::AutoDetect
//! );
//!
//! // With content, the decider can make a decision
//! let text_content = b"Hello, world! This is some text.";
//! assert_eq!(
//!     decider.should_compress(Path::new("document.txt"), Some(text_content)),
//!     CompressionDecision::Compress
//! );
//! ```

mod adaptive;
mod decider;
mod magic;
mod types;

pub use adaptive::AdaptiveCompressor;
pub use decider::CompressionDecider;
pub use magic::{KNOWN_SIGNATURES, MagicSignature};
pub use types::{CompressionDecision, FileCategory};

/// Default size of the sample block for auto-detection (4 KB).
///
/// This size provides a good balance between detection accuracy and overhead.
/// Smaller samples may miss compression patterns, while larger samples waste CPU.
pub const DEFAULT_SAMPLE_SIZE: usize = 4 * 1024;

/// Default compression ratio threshold for auto-detection.
///
/// If the compressed size is >= 90% of the original size, the file is considered
/// incompressible. This threshold matches typical behavior where compression
/// overhead means ratios above 90% provide negligible benefit.
pub const DEFAULT_COMPRESSION_THRESHOLD: f64 = 0.90;

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;

    use crate::zlib::CompressionLevel;

    use super::*;

    #[test]
    fn default_skip_list_includes_common_formats() {
        let decider = CompressionDecider::with_default_skip_list();

        // Images
        assert!(decider.skip_extensions().contains("jpg"));
        assert!(decider.skip_extensions().contains("jpeg"));
        assert!(decider.skip_extensions().contains("png"));
        assert!(decider.skip_extensions().contains("gif"));
        assert!(decider.skip_extensions().contains("webp"));
        assert!(decider.skip_extensions().contains("heic"));

        // Video
        assert!(decider.skip_extensions().contains("mp4"));
        assert!(decider.skip_extensions().contains("mkv"));
        assert!(decider.skip_extensions().contains("avi"));
        assert!(decider.skip_extensions().contains("mov"));
        assert!(decider.skip_extensions().contains("webm"));

        // Audio
        assert!(decider.skip_extensions().contains("mp3"));
        assert!(decider.skip_extensions().contains("flac"));
        assert!(decider.skip_extensions().contains("ogg"));
        assert!(decider.skip_extensions().contains("m4a"));
        assert!(decider.skip_extensions().contains("aac"));

        // Archives
        assert!(decider.skip_extensions().contains("zip"));
        assert!(decider.skip_extensions().contains("gz"));
        assert!(decider.skip_extensions().contains("bz2"));
        assert!(decider.skip_extensions().contains("xz"));
        assert!(decider.skip_extensions().contains("7z"));
        assert!(decider.skip_extensions().contains("rar"));
        assert!(decider.skip_extensions().contains("zst"));

        // Documents
        assert!(decider.skip_extensions().contains("pdf"));
    }

    #[test]
    fn should_compress_skips_known_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("video.mp4"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("archive.zip"), None),
            CompressionDecision::Skip
        );
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
    fn should_compress_unknown_extension_returns_auto_detect() {
        let decider = CompressionDecider::with_default_skip_list();

        assert_eq!(
            decider.should_compress(Path::new("data.xyz"), None),
            CompressionDecision::AutoDetect
        );
    }

    #[test]
    fn should_compress_with_content_makes_decision() {
        let decider = CompressionDecider::with_default_skip_list();

        // Text content should compress
        let text_content = b"Hello, world! This is some compressible text content.";
        assert_eq!(
            decider.should_compress(Path::new("file.txt"), Some(text_content)),
            CompressionDecision::Compress
        );
    }

    #[test]
    fn magic_byte_detection_jpeg() {
        let decider = CompressionDecider::with_default_skip_list();

        // JPEG magic bytes
        let jpeg_header = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&jpeg_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection_png() {
        let decider = CompressionDecider::with_default_skip_list();

        // PNG magic bytes
        let png_header = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&png_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection_gzip() {
        let decider = CompressionDecider::with_default_skip_list();

        // gzip magic bytes
        let gzip_header = [0x1f, 0x8b, 0x08, 0x00];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&gzip_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection_zip() {
        let decider = CompressionDecider::with_default_skip_list();

        // ZIP magic bytes
        let zip_header = [b'P', b'K', 0x03, 0x04];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&zip_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_byte_detection_pdf() {
        let decider = CompressionDecider::with_default_skip_list();

        // PDF magic bytes
        let pdf_header = b"%PDF-1.5";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(pdf_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn parse_skip_compress_list() {
        let decider = CompressionDecider::from_skip_compress_list("txt/log/csv");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn parse_skip_compress_list_with_dots() {
        let decider = CompressionDecider::from_skip_compress_list(".txt/.log/.csv");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn parse_skip_compress_list_whitespace() {
        let decider = CompressionDecider::from_skip_compress_list("txt  log\tcsv");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn auto_detect_compressible_repetitive_data() {
        let decider = CompressionDecider::new();

        // Highly repetitive data should compress well
        let data = vec![b'a'; 4096];
        assert!(decider.auto_detect_compressible(&data).unwrap());
    }

    #[test]
    fn auto_detect_compressible_random_like_data() {
        let decider = CompressionDecider::new();

        // Pseudo-random data using a better mixing function (PCG-like)
        let mut state: u64 = 0x853c49e6748fea9b;
        let data: Vec<u8> = (0..4096)
            .map(|_| {
                // PCG-style PRNG for high-quality randomness
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
                let rot = (state >> 59) as u32;
                ((xorshifted >> rot) | (xorshifted << ((32u32.wrapping_sub(rot)) & 31))) as u8
            })
            .collect();

        // High-entropy data should not compress well (ratio >= threshold)
        assert!(!decider.auto_detect_compressible(&data).unwrap());
    }

    #[test]
    fn auto_detect_compressible_empty() {
        let decider = CompressionDecider::new();
        assert!(decider.auto_detect_compressible(&[]).unwrap());
    }

    #[test]
    fn extract_extension_simple() {
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("file.txt")),
            Some("txt".to_owned())
        );
    }

    #[test]
    fn extract_extension_compound() {
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("archive.tar.gz")),
            Some("tar.gz".to_owned())
        );
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("backup.tar.bz2")),
            Some("tar.bz2".to_owned())
        );
    }

    #[test]
    fn extract_extension_uppercase() {
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("FILE.TXT")),
            Some("txt".to_owned())
        );
    }

    #[test]
    fn extract_extension_no_extension() {
        assert_eq!(
            CompressionDecider::extract_extension(Path::new("Makefile")),
            None
        );
    }

    #[test]
    fn add_remove_extension() {
        let mut decider = CompressionDecider::new();

        decider.add_skip_extension("xyz");
        assert!(decider.skip_extensions().contains("xyz"));

        decider.remove_skip_extension("xyz");
        assert!(!decider.skip_extensions().contains("xyz"));
    }

    #[test]
    fn clear_extensions() {
        let mut decider = CompressionDecider::with_default_skip_list();
        assert!(!decider.skip_extensions().is_empty());

        decider.clear_skip_extensions();
        assert!(decider.skip_extensions().is_empty());
    }

    #[test]
    fn set_compression_threshold() {
        let mut decider = CompressionDecider::new();
        decider.set_compression_threshold(0.85);
        assert!((decider.compression_threshold() - 0.85).abs() < f64::EPSILON);

        // Test clamping
        decider.set_compression_threshold(1.5);
        assert!((decider.compression_threshold() - 1.0).abs() < f64::EPSILON);

        decider.set_compression_threshold(-0.5);
        assert!((decider.compression_threshold() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn set_sample_size() {
        let mut decider = CompressionDecider::new();
        decider.set_sample_size(8192);
        assert_eq!(decider.sample_size(), 8192);

        // Test minimum
        decider.set_sample_size(10);
        assert_eq!(decider.sample_size(), 64); // Minimum is 64
    }

    #[test]
    fn toggle_magic_detection() {
        let mut decider = CompressionDecider::new();
        assert!(decider.use_magic_detection());

        decider.set_use_magic_detection(false);
        assert!(!decider.use_magic_detection());

        // With magic detection disabled, JPEG header should not trigger skip
        let jpeg_header = [0xff, 0xd8, 0xff, 0xe0];
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(&jpeg_header)),
            CompressionDecision::Compress
        );
    }

    #[test]
    fn file_category_compressible() {
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
    fn riff_container_detection() {
        let decider = CompressionDecider::with_default_skip_list();

        // AVI (RIFF....AVI )
        let avi_header = b"RIFF\x00\x00\x00\x00AVI ";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(avi_header)),
            CompressionDecision::Skip
        );

        // WAV (RIFF....WAVE)
        let wav_header = b"RIFF\x00\x00\x00\x00WAVE";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(wav_header)),
            CompressionDecision::Skip
        );

        // WEBP (RIFF....WEBP)
        let webp_header = b"RIFF\x00\x00\x00\x00WEBP";
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(webp_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn magic_signature_matches() {
        let sig = MagicSignature::new(0, b"TEST", FileCategory::Unknown);
        assert!(sig.matches(b"TEST1234"));
        assert!(!sig.matches(b"NOPE1234"));
        assert!(!sig.matches(b"TES")); // Too short
    }

    #[test]
    fn magic_signature_with_offset() {
        let sig = MagicSignature::new(4, b"DATA", FileCategory::Unknown);
        assert!(sig.matches(b"XXXXDATAYYY"));
        assert!(!sig.matches(b"DATXXXXX"));
    }

    #[test]
    fn adaptive_compressor_basic() {
        let decider = CompressionDecider::new();
        let mut output = Vec::new();
        let mut compressor = AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Fast);

        // Write compressible data
        let data = vec![b'a'; 8192];
        compressor.write_all(&data).unwrap();

        let output = compressor.finish().unwrap();

        // Compressible data should result in smaller output
        assert!(output.len() < data.len());
    }

    #[test]
    fn adaptive_compressor_forced_decision() {
        let decider = CompressionDecider::new();
        let mut output = Vec::new();
        let mut compressor = AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Fast);

        // Force no compression
        compressor.set_decision(false);
        assert_eq!(compressor.compression_enabled(), Some(false));

        let data = b"test data";
        compressor.write_all(data).unwrap();
        let output = compressor.finish().unwrap();

        // Output should be identical to input (no compression)
        assert_eq!(&output[..], data);
    }

    #[test]
    fn adaptive_compressor_small_write() {
        let mut decider = CompressionDecider::new();
        decider.set_sample_size(100);

        let mut output = Vec::new();
        let mut compressor =
            AdaptiveCompressor::new(&mut output, decider, CompressionLevel::Default);

        // Write less than sample size
        compressor.write_all(b"small").unwrap();

        // Decision shouldn't be made yet
        assert_eq!(compressor.compression_enabled(), None);

        // Finish should still work
        let _ = compressor.finish().unwrap();
    }

    #[test]
    fn compression_decider_default() {
        let decider = CompressionDecider::default();
        assert!(!decider.skip_extensions().is_empty());
    }
}
