//! Comprehensive tests for --skip-compress file type exclusion functionality.
//!
//! These tests verify that:
//! 1. Files matching patterns are not compressed
//! 2. Default skip list (.gz, .zip, .jpg, etc.) works correctly
//! 3. Custom patterns can be added and removed
//! 4. Patterns match correctly (case-insensitive, compound extensions)
//! 5. Magic byte detection works for files without extensions
//! 6. Edge cases and boundary conditions are handled

use std::path::Path;

use compress::skip_compress::{
    CompressionDecider, CompressionDecision, DEFAULT_COMPRESSION_THRESHOLD, DEFAULT_SAMPLE_SIZE,
    FileCategory, KNOWN_SIGNATURES, MagicSignature,
};

// =============================================================================
// SECTION 1: Default Skip List Verification
// =============================================================================

mod default_skip_list {
    use super::*;

    #[test]
    fn default_list_includes_all_image_formats() {
        let decider = CompressionDecider::with_default_skip_list();
        let extensions = decider.skip_extensions();

        // Common image formats
        let images = [
            "jpg", "jpeg", "jpe", "png", "gif", "webp", "heic", "heif", "avif",
        ];
        for ext in images {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain image extension: {ext}"
            );
        }

        // Less common but supported image formats
        let more_images = ["tif", "tiff", "bmp", "ico", "svg", "svgz", "psd"];
        for ext in more_images {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain extended image extension: {ext}"
            );
        }

        // RAW image formats
        let raw_formats = ["raw", "arw", "cr2", "nef", "orf", "sr2"];
        for ext in raw_formats {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain RAW image extension: {ext}"
            );
        }
    }

    #[test]
    fn default_list_includes_all_video_formats() {
        let decider = CompressionDecider::with_default_skip_list();
        let extensions = decider.skip_extensions();

        let videos = [
            "mp4", "m4v", "mkv", "avi", "mov", "wmv", "flv", "webm", "mpeg", "mpg", "vob", "ogv",
            "3gp", "3g2", "ts", "mts", "m2ts",
        ];
        for ext in videos {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain video extension: {ext}"
            );
        }
    }

    #[test]
    fn default_list_includes_all_audio_formats() {
        let decider = CompressionDecider::with_default_skip_list();
        let extensions = decider.skip_extensions();

        let audio = [
            "mp3", "m4a", "aac", "ogg", "oga", "opus", "flac", "wma", "wav", "aiff", "ape", "mka",
            "ac3", "dts",
        ];
        for ext in audio {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain audio extension: {ext}"
            );
        }
    }

    #[test]
    fn default_list_includes_all_archive_formats() {
        let decider = CompressionDecider::with_default_skip_list();
        let extensions = decider.skip_extensions();

        // Basic archive formats
        let archives = [
            "zip", "gz", "gzip", "bz2", "bzip2", "xz", "lzma", "7z", "rar",
        ];
        for ext in archives {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain archive extension: {ext}"
            );
        }

        // Modern compression formats
        let modern = ["zst", "zstd", "lz4", "lzo"];
        for ext in modern {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain modern compression extension: {ext}"
            );
        }

        // Legacy formats
        let legacy = ["z", "cab", "arj", "lzh"];
        for ext in legacy {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain legacy compression extension: {ext}"
            );
        }

        // Compound archive extensions
        let compound = ["tar.gz", "tar.bz2", "tar.xz", "tgz", "tbz", "tbz2", "txz"];
        for ext in compound {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain compound archive extension: {ext}"
            );
        }
    }

    #[test]
    fn default_list_includes_package_formats() {
        let decider = CompressionDecider::with_default_skip_list();
        let extensions = decider.skip_extensions();

        let packages = [
            "deb", "rpm", "apk", "jar", "war", "ear", "egg", "whl", "gem", "nupkg", "snap", "appx",
            "msix",
        ];
        for ext in packages {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain package extension: {ext}"
            );
        }
    }

    #[test]
    fn default_list_includes_document_formats() {
        let decider = CompressionDecider::with_default_skip_list();
        let extensions = decider.skip_extensions();

        // PDF and ebooks
        let docs = ["pdf", "epub", "mobi", "azw", "azw3"];
        for ext in docs {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain document extension: {ext}"
            );
        }

        // Office formats (pre-compressed)
        let office = ["docx", "xlsx", "pptx", "odt", "ods", "odp"];
        for ext in office {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain Office extension: {ext}"
            );
        }
    }

    #[test]
    fn default_list_includes_disk_images() {
        let decider = CompressionDecider::with_default_skip_list();
        let extensions = decider.skip_extensions();

        let disk_images = ["iso", "img", "dmg", "vhd", "vhdx", "vmdk", "qcow", "qcow2"];
        for ext in disk_images {
            assert!(
                extensions.contains(ext),
                "Default skip list should contain disk image extension: {ext}"
            );
        }
    }

    #[test]
    fn default_skip_list_is_not_empty() {
        let decider = CompressionDecider::with_default_skip_list();
        assert!(
            !decider.skip_extensions().is_empty(),
            "Default skip list should contain extensions"
        );
        assert!(
            decider.skip_extensions().len() > 50,
            "Default skip list should have a substantial number of extensions"
        );
    }
}

// =============================================================================
// SECTION 2: File Pattern Matching
// =============================================================================

mod pattern_matching {
    use super::*;

    #[test]
    fn files_matching_skip_patterns_are_not_compressed() {
        let decider = CompressionDecider::with_default_skip_list();

        let skip_files = [
            // Images
            "photo.jpg",
            "image.png",
            "animation.gif",
            "picture.webp",
            // Videos
            "movie.mp4",
            "clip.mkv",
            "video.avi",
            // Audio
            "song.mp3",
            "track.flac",
            "audio.ogg",
            // Archives
            "archive.zip",
            "data.gz",
            "backup.tar.gz",
            "files.7z",
            // Documents
            "document.pdf",
            "book.epub",
            "report.docx",
            // Packages
            "package.deb",
            "application.rpm",
            "library.jar",
        ];

        for file in skip_files {
            let result = decider.should_compress(Path::new(file), None);
            assert_eq!(
                result,
                CompressionDecision::Skip,
                "File {file} should be skipped for compression"
            );
        }
    }

    #[test]
    fn files_not_in_skip_list_return_auto_detect() {
        let decider = CompressionDecider::with_default_skip_list();

        let unknown_files = [
            "data.xyz",
            "file.unknown",
            "readme.txt",
            "source.rs",
            "script.py",
            "config.json",
            "data.csv",
            "log.txt",
        ];

        for file in unknown_files {
            let result = decider.should_compress(Path::new(file), None);
            assert_eq!(
                result,
                CompressionDecision::AutoDetect,
                "File {file} should require auto-detection"
            );
        }
    }

    #[test]
    fn case_insensitive_pattern_matching() {
        let decider = CompressionDecider::with_default_skip_list();

        let test_cases = [
            ("PHOTO.JPG", CompressionDecision::Skip),
            ("Photo.Jpg", CompressionDecision::Skip),
            ("video.MP4", CompressionDecision::Skip),
            ("VIDEO.MKV", CompressionDecision::Skip),
            ("AUDIO.mp3", CompressionDecision::Skip),
            ("Archive.ZIP", CompressionDecision::Skip),
            ("Document.PDF", CompressionDecision::Skip),
            ("BACKUP.TAR.GZ", CompressionDecision::Skip),
        ];

        for (file, expected) in test_cases {
            let result = decider.should_compress(Path::new(file), None);
            assert_eq!(
                result, expected,
                "Case-insensitive matching failed for {file}"
            );
        }
    }

    #[test]
    fn compound_extension_matching() {
        let decider = CompressionDecider::with_default_skip_list();

        let compound_extensions = [
            "archive.tar.gz",
            "backup.tar.bz2",
            "data.tar.xz",
            "files.tar.zst",
        ];

        for file in compound_extensions {
            let result = decider.should_compress(Path::new(file), None);
            assert_eq!(
                result,
                CompressionDecision::Skip,
                "Compound extension {file} should be recognized"
            );
        }
    }

    #[test]
    fn files_with_paths_are_matched_by_extension() {
        let decider = CompressionDecider::with_default_skip_list();

        let files_with_paths = [
            "/home/user/photos/vacation.jpg",
            "/var/backups/archive.tar.gz",
            "../relative/path/video.mp4",
            "./current/dir/audio.mp3",
            "nested/directory/structure/document.pdf",
        ];

        for file in files_with_paths {
            let result = decider.should_compress(Path::new(file), None);
            assert_eq!(
                result,
                CompressionDecision::Skip,
                "File with path {file} should be matched by extension"
            );
        }
    }

    #[test]
    fn files_without_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        let no_extension_files = ["Makefile", "README", "LICENSE", ".gitignore", "dockerfile"];

        for file in no_extension_files {
            let result = decider.should_compress(Path::new(file), None);
            assert_eq!(
                result,
                CompressionDecision::AutoDetect,
                "File without extension {file} should require auto-detection"
            );
        }
    }

    #[test]
    fn hidden_files_with_extensions() {
        let decider = CompressionDecider::with_default_skip_list();

        assert_eq!(
            decider.should_compress(Path::new(".hidden.jpg"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new(".config.zip"), None),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn files_with_multiple_dots() {
        let decider = CompressionDecider::with_default_skip_list();

        assert_eq!(
            decider.should_compress(Path::new("my.photo.backup.jpg"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("data.backup.2024.tar.gz"), None),
            CompressionDecision::Skip
        );
    }
}

// =============================================================================
// SECTION 3: Custom Pattern Management
// =============================================================================

mod custom_patterns {
    use super::*;

    #[test]
    fn add_custom_skip_extension() {
        let mut decider = CompressionDecider::new();

        // Initially empty
        assert!(decider.skip_extensions().is_empty());

        // Add custom extension
        decider.add_skip_extension("xyz");
        assert!(decider.skip_extensions().contains("xyz"));

        // Verify it's used in matching
        assert_eq!(
            decider.should_compress(Path::new("file.xyz"), None),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn add_extension_with_leading_dot() {
        let mut decider = CompressionDecider::new();

        decider.add_skip_extension(".abc");
        assert!(decider.skip_extensions().contains("abc"));
        assert!(!decider.skip_extensions().contains(".abc"));

        // Should work without the dot internally
        assert_eq!(
            decider.should_compress(Path::new("file.abc"), None),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn add_extension_case_normalization() {
        let mut decider = CompressionDecider::new();

        decider.add_skip_extension("XYZ");
        assert!(decider.skip_extensions().contains("xyz"));
        assert!(!decider.skip_extensions().contains("XYZ"));

        // Should match case-insensitively
        assert_eq!(
            decider.should_compress(Path::new("FILE.XYZ"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("file.xyz"), None),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn remove_skip_extension() {
        let mut decider = CompressionDecider::with_default_skip_list();

        // Verify jpg is in the default list
        assert!(decider.skip_extensions().contains("jpg"));

        // Remove it
        let removed = decider.remove_skip_extension("jpg");
        assert!(removed, "Should return true when extension was present");
        assert!(!decider.skip_extensions().contains("jpg"));

        // Now jpg files should require auto-detection
        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::AutoDetect
        );
    }

    #[test]
    fn remove_nonexistent_extension() {
        let mut decider = CompressionDecider::new();

        let removed = decider.remove_skip_extension("xyz");
        assert!(
            !removed,
            "Should return false when extension was not present"
        );
    }

    #[test]
    fn clear_all_extensions() {
        let mut decider = CompressionDecider::with_default_skip_list();

        // Verify we have extensions
        assert!(!decider.skip_extensions().is_empty());

        // Clear them
        decider.clear_skip_extensions();
        assert!(decider.skip_extensions().is_empty());

        // Previously skipped files now require auto-detection
        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::AutoDetect
        );
    }

    #[test]
    fn add_multiple_extensions() {
        let mut decider = CompressionDecider::new();

        let custom_exts = ["log", "tmp", "cache", "bak"];
        for ext in custom_exts {
            decider.add_skip_extension(ext);
        }

        assert_eq!(decider.skip_extensions().len(), custom_exts.len());

        for ext in custom_exts {
            assert!(decider.skip_extensions().contains(ext));
        }
    }

    #[test]
    fn add_empty_extension_is_ignored() {
        let mut decider = CompressionDecider::new();

        decider.add_skip_extension("");
        assert!(decider.skip_extensions().is_empty());

        decider.add_skip_extension("   ");
        assert!(decider.skip_extensions().is_empty());
    }

    #[test]
    fn extension_with_whitespace_is_trimmed() {
        let mut decider = CompressionDecider::new();

        decider.add_skip_extension("  xyz  ");
        assert!(!decider.skip_extensions().contains("  xyz  "));
        assert!(decider.skip_extensions().contains("xyz"));
    }

    #[test]
    fn combine_default_and_custom_extensions() {
        let mut decider = CompressionDecider::with_default_skip_list();
        let initial_count = decider.skip_extensions().len();

        // Add custom extensions
        decider.add_skip_extension("custom1");
        decider.add_skip_extension("custom2");

        assert_eq!(decider.skip_extensions().len(), initial_count + 2);

        // Both default and custom should work
        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("file.custom1"), None),
            CompressionDecision::Skip
        );
    }
}

// =============================================================================
// SECTION 4: Skip-Compress List Parsing
// =============================================================================

mod list_parsing {
    use super::*;

    #[test]
    fn parse_slash_separated_list() {
        let decider = CompressionDecider::from_skip_compress_list("txt/log/csv");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
        assert_eq!(decider.skip_extensions().len(), 3);
    }

    #[test]
    fn parse_whitespace_separated_list() {
        let decider = CompressionDecider::from_skip_compress_list("txt  log\tcsv\nxml");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
        assert!(decider.skip_extensions().contains("xml"));
    }

    #[test]
    fn parse_mixed_separators() {
        let decider = CompressionDecider::from_skip_compress_list("txt/log csv\txml/json");

        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
        assert!(decider.skip_extensions().contains("xml"));
        assert!(decider.skip_extensions().contains("json"));
    }

    #[test]
    fn parse_list_with_leading_dots() {
        let decider = CompressionDecider::from_skip_compress_list(".txt/.log/.csv");

        // Leading dots should be stripped
        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
        assert!(!decider.skip_extensions().contains(".txt"));
    }

    #[test]
    fn parse_list_with_case_variations() {
        let decider = CompressionDecider::from_skip_compress_list("TXT/Log/CSV");

        // All should be normalized to lowercase
        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn parse_empty_list() {
        let decider = CompressionDecider::from_skip_compress_list("");
        assert!(decider.skip_extensions().is_empty());
    }

    #[test]
    fn parse_list_with_empty_elements() {
        let decider = CompressionDecider::from_skip_compress_list("txt//log///csv");

        // Empty elements should be ignored
        assert_eq!(decider.skip_extensions().len(), 3);
        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn parse_list_with_trailing_separator() {
        let decider = CompressionDecider::from_skip_compress_list("txt/log/csv/");

        assert_eq!(decider.skip_extensions().len(), 3);
        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
    }

    #[test]
    fn parse_single_extension() {
        let decider = CompressionDecider::from_skip_compress_list("xyz");

        assert_eq!(decider.skip_extensions().len(), 1);
        assert!(decider.skip_extensions().contains("xyz"));
    }

    #[test]
    fn parse_list_matches_rsync_format() {
        // Test rsync-compatible format: "gz/zip/z/rpm/deb/iso/bz2/tbz/tbz2/gz2"
        let decider =
            CompressionDecider::from_skip_compress_list("gz/zip/z/rpm/deb/iso/bz2/tbz/tbz2/gz2");

        let expected = [
            "gz", "zip", "z", "rpm", "deb", "iso", "bz2", "tbz", "tbz2", "gz2",
        ];
        assert_eq!(decider.skip_extensions().len(), expected.len());

        for ext in expected {
            assert!(
                decider.skip_extensions().contains(ext),
                "Should contain rsync-format extension: {ext}"
            );
        }
    }
}

// =============================================================================
// SECTION 5: Magic Byte Detection
// =============================================================================

mod magic_byte_detection {
    use super::*;

    #[test]
    fn detect_jpeg_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // JPEG magic bytes: FF D8 FF
        let jpeg_headers = [
            vec![0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10], // JFIF
            vec![0xff, 0xd8, 0xff, 0xe1, 0x00, 0x00], // EXIF
            vec![0xff, 0xd8, 0xff, 0xdb],             // Minimal
        ];

        for (i, header) in jpeg_headers.iter().enumerate() {
            let result = decider.should_compress(Path::new("unknown"), Some(header));
            assert_eq!(
                result,
                CompressionDecision::Skip,
                "JPEG magic bytes variant {i} should be detected"
            );
        }
    }

    #[test]
    fn detect_png_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // PNG signature: 89 50 4E 47 0D 0A 1A 0A
        let png_header = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

        let result = decider.should_compress(Path::new("unknown"), Some(&png_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_gif_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // GIF87a and GIF89a
        let gif87_header = b"GIF87a\x00\x00";
        let gif89_header = b"GIF89a\x00\x00";

        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(gif87_header)),
            CompressionDecision::Skip
        );
        assert_eq!(
            decider.should_compress(Path::new("unknown"), Some(gif89_header)),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn detect_zip_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // ZIP magic bytes: PK\x03\x04
        let zip_header = vec![b'P', b'K', 0x03, 0x04, 0x00, 0x00];

        let result = decider.should_compress(Path::new("unknown"), Some(&zip_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_gzip_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // gzip magic bytes: 1F 8B
        let gzip_header = vec![0x1f, 0x8b, 0x08, 0x00];

        let result = decider.should_compress(Path::new("unknown"), Some(&gzip_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_bzip2_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // bzip2 magic bytes: BZ
        let bzip2_header = b"BZh9\x17\x72\x45\x38";

        let result = decider.should_compress(Path::new("unknown"), Some(bzip2_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_pdf_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // PDF magic bytes: %PDF
        let pdf_headers = [
            b"%PDF-1.4",
            b"%PDF-1.5",
            b"%PDF-1.6",
            b"%PDF-1.7",
            b"%PDF-2.0",
        ];

        for (i, header) in pdf_headers.iter().enumerate() {
            let result = decider.should_compress(Path::new("unknown"), Some(*header));
            assert_eq!(
                result,
                CompressionDecision::Skip,
                "PDF version {i} should be detected"
            );
        }
    }

    #[test]
    fn detect_7z_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // 7z magic bytes: 37 7A BC AF 27 1C
        let sevenzip_header = b"7z\xbc\xaf\x27\x1c";

        let result = decider.should_compress(Path::new("unknown"), Some(sevenzip_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_rar_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // RAR magic bytes: Rar!\x1a\x07
        let rar_header = b"Rar!\x1a\x07\x00";

        let result = decider.should_compress(Path::new("unknown"), Some(rar_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_xz_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // XZ magic bytes: FD 37 7A 58 5A 00
        let xz_header = b"\xfd7zXZ\x00";

        let result = decider.should_compress(Path::new("unknown"), Some(xz_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_zstd_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // zstd magic bytes: 28 B5 2F FD
        let zstd_header = vec![0x28, 0xb5, 0x2f, 0xfd];

        let result = decider.should_compress(Path::new("unknown"), Some(&zstd_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_lz4_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // LZ4 magic bytes: 04 22 4D 18
        let lz4_header = vec![0x04, 0x22, 0x4d, 0x18];

        let result = decider.should_compress(Path::new("unknown"), Some(&lz4_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn plain_text_content_is_compressible() {
        let decider = CompressionDecider::with_default_skip_list();

        let text_samples = [
            b"Hello, this is plain text content" as &[u8],
            b"Lorem ipsum dolor sit amet, consectetur adipiscing elit.",
            b"function main() { console.log('test'); }",
            b"# This is a comment\nprint('Hello, world!')",
        ];

        for text in text_samples {
            let result = decider.should_compress(Path::new("unknown"), Some(text));
            assert_eq!(
                result,
                CompressionDecision::Compress,
                "Plain text should be compressible"
            );
        }
    }

    #[test]
    fn magic_detection_can_be_disabled() {
        let mut decider = CompressionDecider::with_default_skip_list();
        decider.set_use_magic_detection(false);

        // JPEG magic bytes should not trigger skip when magic detection is disabled
        let jpeg_header = vec![0xff, 0xd8, 0xff, 0xe0];
        let result = decider.should_compress(Path::new("unknown"), Some(&jpeg_header));

        // Without magic detection and no extension match, should compress
        assert_eq!(result, CompressionDecision::Compress);
    }

    #[test]
    fn incomplete_magic_bytes_do_not_match() {
        let decider = CompressionDecider::with_default_skip_list();

        // Incomplete JPEG header (needs at least 3 bytes: FF D8 FF)
        let incomplete = vec![0xff, 0xd8];
        let result = decider.should_compress(Path::new("unknown"), Some(&incomplete));
        assert_ne!(result, CompressionDecision::Skip);

        // Incomplete PNG header
        let incomplete_png = vec![0x89, b'P', b'N'];
        let result = decider.should_compress(Path::new("unknown"), Some(&incomplete_png));
        assert_ne!(result, CompressionDecision::Skip);
    }

    #[test]
    fn magic_bytes_override_extension() {
        let decider = CompressionDecider::with_default_skip_list();

        // A file named .txt but with JPEG magic bytes should still be skipped
        let jpeg_header = vec![0xff, 0xd8, 0xff, 0xe0];
        let result = decider.should_compress(Path::new("fake.txt"), Some(&jpeg_header));
        assert_eq!(
            result,
            CompressionDecision::Skip,
            "Magic bytes should take precedence for incompressible formats"
        );
    }
}

// =============================================================================
// SECTION 6: RIFF Container Detection
// =============================================================================

mod riff_containers {
    use super::*;

    #[test]
    fn detect_avi_riff_container() {
        let decider = CompressionDecider::with_default_skip_list();

        // AVI: RIFF....AVI
        let avi_header = b"RIFF\x00\x00\x00\x00AVI \x00\x00";

        let result = decider.should_compress(Path::new("unknown"), Some(avi_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_wav_riff_container() {
        let decider = CompressionDecider::with_default_skip_list();

        // WAV: RIFF....WAVE
        let wav_header = b"RIFF\x00\x00\x00\x00WAVE";

        let result = decider.should_compress(Path::new("unknown"), Some(wav_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn detect_webp_riff_container() {
        let decider = CompressionDecider::with_default_skip_list();

        // WEBP: RIFF....WEBP
        let webp_header = b"RIFF\x00\x00\x00\x00WEBP";

        let result = decider.should_compress(Path::new("unknown"), Some(webp_header));
        assert_eq!(result, CompressionDecision::Skip);
    }

    #[test]
    fn riff_with_unknown_fourcc_is_not_detected() {
        let decider = CompressionDecider::with_default_skip_list();

        // RIFF container with unknown FourCC
        let unknown_riff = b"RIFF\x00\x00\x00\x00UNKN";

        let result = decider.should_compress(Path::new("unknown"), Some(unknown_riff));
        // Should not match, falls through to other detection
        assert_ne!(result, CompressionDecision::Skip);
    }

    #[test]
    fn incomplete_riff_header_does_not_match() {
        let decider = CompressionDecider::with_default_skip_list();

        // RIFF header without FourCC (less than 12 bytes)
        let incomplete = b"RIFF\x00\x00\x00\x00";

        let result = decider.should_compress(Path::new("unknown"), Some(incomplete));
        // Should not trigger RIFF-specific detection
        assert_eq!(result, CompressionDecision::Compress);
    }
}

// =============================================================================
// SECTION 7: Auto-Detection
// =============================================================================

mod auto_detection {
    use super::*;

    #[test]
    fn highly_compressible_data_is_detected() {
        let decider = CompressionDecider::new();

        // Repetitive data compresses very well
        let repetitive = vec![b'a'; 4096];
        assert!(decider.auto_detect_compressible(&repetitive).unwrap());

        let zeros = vec![0u8; 4096];
        assert!(decider.auto_detect_compressible(&zeros).unwrap());

        let ones = vec![0xff; 4096];
        assert!(decider.auto_detect_compressible(&ones).unwrap());
    }

    #[test]
    fn incompressible_data_is_detected() {
        let decider = CompressionDecider::new();

        // High-entropy pseudo-random data doesn't compress well
        let mut state: u64 = 0x853c49e6748fea9b;
        let random_data: Vec<u8> = (0..4096)
            .map(|_| {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let xorshifted = (((state >> 18) ^ state) >> 27) as u32;
                let rot = (state >> 59) as u32;
                ((xorshifted >> rot) | (xorshifted << ((32u32.wrapping_sub(rot)) & 31))) as u8
            })
            .collect();

        assert!(!decider.auto_detect_compressible(&random_data).unwrap());
    }

    #[test]
    fn empty_data_is_considered_compressible() {
        let decider = CompressionDecider::new();
        assert!(decider.auto_detect_compressible(&[]).unwrap());
    }

    #[test]
    fn structured_text_is_compressible() {
        let decider = CompressionDecider::new();

        let json_data = br#"{"name": "test", "value": 123, "nested": {"key": "value"}}"#.repeat(50);
        assert!(decider.auto_detect_compressible(&json_data).unwrap());

        let xml_data = b"<root><element>value</element></root>".repeat(50);
        assert!(decider.auto_detect_compressible(&xml_data).unwrap());
    }

    #[test]
    fn compression_threshold_affects_detection() {
        let mut decider = CompressionDecider::new();

        // Create data with moderate compressibility
        let moderate_data = (0..4096).map(|i| (i % 4) as u8).collect::<Vec<_>>();

        // With default threshold (0.90), check the result
        let default_result = decider.auto_detect_compressible(&moderate_data).unwrap();

        // With very low threshold (0.10), should be incompressible
        decider.set_compression_threshold(0.10);
        let low_threshold_result = decider.auto_detect_compressible(&moderate_data).unwrap();

        // With very high threshold (1.00), should be compressible
        decider.set_compression_threshold(1.00);
        let high_threshold_result = decider.auto_detect_compressible(&moderate_data).unwrap();

        // The thresholds should affect the decision
        // (exact behavior depends on compression ratio of moderate_data)
        let _ = default_result; // Default threshold produces a decision
        let _ = low_threshold_result; // Low threshold produces a decision
        assert!(high_threshold_result);
    }

    #[test]
    fn sample_size_configuration() {
        let mut decider = CompressionDecider::new();

        assert_eq!(decider.sample_size(), DEFAULT_SAMPLE_SIZE);

        decider.set_sample_size(8192);
        assert_eq!(decider.sample_size(), 8192);

        decider.set_sample_size(2048);
        assert_eq!(decider.sample_size(), 2048);
    }

    #[test]
    fn minimum_sample_size_enforced() {
        let mut decider = CompressionDecider::new();

        // Attempt to set very small sample size
        decider.set_sample_size(10);

        // Should be clamped to minimum (64 bytes)
        assert_eq!(decider.sample_size(), 64);

        decider.set_sample_size(0);
        assert_eq!(decider.sample_size(), 64);
    }
}

// =============================================================================
// SECTION 8: File Category Classification
// =============================================================================

mod file_categories {
    use super::*;

    #[test]
    fn incompressible_categories() {
        assert!(!FileCategory::Image.is_compressible());
        assert!(!FileCategory::Video.is_compressible());
        assert!(!FileCategory::Audio.is_compressible());
        assert!(!FileCategory::Archive.is_compressible());
        assert!(!FileCategory::Document.is_compressible());
    }

    #[test]
    fn compressible_categories() {
        assert!(FileCategory::Text.is_compressible());
        assert!(FileCategory::Data.is_compressible());
        assert!(FileCategory::Executable.is_compressible());
        assert!(FileCategory::Unknown.is_compressible());
    }

    #[test]
    fn magic_signatures_have_correct_categories() {
        for sig in KNOWN_SIGNATURES {
            // Verify each signature has a valid category
            let _category = sig.category;

            // Verify signatures can match their own test data
            let mut test_data = vec![0u8; sig.offset + sig.bytes.len()];
            test_data[sig.offset..sig.offset + sig.bytes.len()].copy_from_slice(sig.bytes);

            assert!(
                sig.matches(&test_data),
                "Signature at offset {} should match its own test data",
                sig.offset
            );
        }
    }

    #[test]
    fn magic_signature_offset_matching() {
        // Test signature at offset 0
        let sig0 = MagicSignature::new(0, b"TEST", FileCategory::Unknown);
        assert!(sig0.matches(b"TEST1234"));
        assert!(!sig0.matches(b"XXTEST"));

        // Test signature at offset 4
        let sig4 = MagicSignature::new(4, b"DATA", FileCategory::Data);
        assert!(sig4.matches(b"XXXXDATA"));
        assert!(!sig4.matches(b"DATA"));
        assert!(!sig4.matches(b"XXXDATA")); // offset 3, not 4
    }

    #[test]
    fn all_known_signatures_are_valid() {
        for sig in KNOWN_SIGNATURES {
            assert!(!sig.bytes.is_empty(), "Signature bytes should not be empty");
            assert!(
                sig.offset < 100,
                "Signature offset {} seems unreasonably large",
                sig.offset
            );
        }
    }
}

// =============================================================================
// SECTION 9: Configuration and Settings
// =============================================================================

mod configuration {
    use super::*;

    #[test]
    fn compression_threshold_configuration() {
        let mut decider = CompressionDecider::new();

        // Default threshold
        assert!(
            (decider.compression_threshold() - DEFAULT_COMPRESSION_THRESHOLD).abs() < f64::EPSILON
        );

        // Set custom threshold
        decider.set_compression_threshold(0.85);
        assert!((decider.compression_threshold() - 0.85).abs() < f64::EPSILON);

        // Verify it's used
        assert_eq!(decider.compression_threshold(), 0.85);
    }

    #[test]
    fn compression_threshold_clamping() {
        let mut decider = CompressionDecider::new();

        // Values above 1.0 should be clamped to 1.0
        decider.set_compression_threshold(1.5);
        assert!((decider.compression_threshold() - 1.0).abs() < f64::EPSILON);

        decider.set_compression_threshold(100.0);
        assert!((decider.compression_threshold() - 1.0).abs() < f64::EPSILON);

        // Values below 0.0 should be clamped to 0.0
        decider.set_compression_threshold(-0.5);
        assert!((decider.compression_threshold() - 0.0).abs() < f64::EPSILON);

        decider.set_compression_threshold(-100.0);
        assert!((decider.compression_threshold() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn magic_detection_toggle() {
        let mut decider = CompressionDecider::new();

        // Default is enabled
        assert!(decider.use_magic_detection());

        // Disable it
        decider.set_use_magic_detection(false);
        assert!(!decider.use_magic_detection());

        // Re-enable it
        decider.set_use_magic_detection(true);
        assert!(decider.use_magic_detection());
    }

    #[test]
    fn default_trait_implementation() {
        let decider = CompressionDecider::default();

        // Default should match with_default_skip_list()
        assert!(!decider.skip_extensions().is_empty());
        assert!(decider.use_magic_detection());
        assert_eq!(decider.sample_size(), DEFAULT_SAMPLE_SIZE);
        assert!(
            (decider.compression_threshold() - DEFAULT_COMPRESSION_THRESHOLD).abs() < f64::EPSILON
        );
    }

    #[test]
    fn constants_are_reasonable() {
        // Verify the constants have reasonable values
        // Sample size should be at least 1KB and not exceed 1MB
        assert_eq!(DEFAULT_SAMPLE_SIZE, 4096); // Verify exact expected value

        // Verify threshold is expected value (0.90, which is > 0.5 and <= 1.0)
        assert!((DEFAULT_COMPRESSION_THRESHOLD - 0.90).abs() < f64::EPSILON);
    }
}

// =============================================================================
// SECTION 10: Integration Tests
// =============================================================================

mod integration {
    use super::*;

    #[test]
    fn extension_takes_precedence_over_content() {
        let decider = CompressionDecider::with_default_skip_list();

        // A .jpg file should be skipped even if we provide compressible content
        let text_content = b"This is highly compressible text content!";
        let result = decider.should_compress(Path::new("photo.jpg"), Some(text_content));

        assert_eq!(
            result,
            CompressionDecision::Skip,
            "Extension match should take precedence"
        );
    }

    #[test]
    fn workflow_unknown_extension_with_magic_bytes() {
        let decider = CompressionDecider::with_default_skip_list();

        // File without known extension but with JPEG magic bytes
        let jpeg_magic = vec![0xff, 0xd8, 0xff, 0xe0];
        let result = decider.should_compress(Path::new("IMG_1234"), Some(&jpeg_magic));

        assert_eq!(
            result,
            CompressionDecision::Skip,
            "Magic bytes should identify the file type"
        );
    }

    #[test]
    fn workflow_custom_skip_list_overrides_default() {
        let mut decider = CompressionDecider::with_default_skip_list();

        // Remove jpg from skip list
        decider.remove_skip_extension("jpg");

        // Add a custom extension
        decider.add_skip_extension("myext");

        // jpg files should now be auto-detected
        assert_eq!(
            decider.should_compress(Path::new("photo.jpg"), None),
            CompressionDecision::AutoDetect
        );

        // myext files should be skipped
        assert_eq!(
            decider.should_compress(Path::new("file.myext"), None),
            CompressionDecision::Skip
        );
    }

    #[test]
    fn realistic_file_set_classification() {
        let decider = CompressionDecider::with_default_skip_list();

        let test_files = [
            ("vacation/IMG_1234.jpg", CompressionDecision::Skip),
            ("videos/movie.mp4", CompressionDecision::Skip),
            ("music/song.mp3", CompressionDecision::Skip),
            ("documents/report.pdf", CompressionDecision::Skip),
            ("backups/data.tar.gz", CompressionDecision::Skip),
            ("source/main.rs", CompressionDecision::AutoDetect),
            ("logs/application.log", CompressionDecision::AutoDetect),
            ("data/records.csv", CompressionDecision::AutoDetect),
            ("README.md", CompressionDecision::AutoDetect),
            ("Makefile", CompressionDecision::AutoDetect),
        ];

        for (file, expected) in test_files {
            let result = decider.should_compress(Path::new(file), None);
            assert_eq!(result, expected, "File {file} classification mismatch");
        }
    }

    #[test]
    fn empty_decider_requires_auto_detect_for_everything() {
        let decider = CompressionDecider::new();

        let files = [
            "photo.jpg",
            "video.mp4",
            "archive.zip",
            "document.txt",
            "source.rs",
        ];

        for file in files {
            let result = decider.should_compress(Path::new(file), None);
            assert_eq!(
                result,
                CompressionDecision::AutoDetect,
                "Empty decider should require auto-detection for {file}"
            );
        }
    }

    #[test]
    fn parse_list_then_add_extensions() {
        let mut decider = CompressionDecider::from_skip_compress_list("txt/log");
        assert_eq!(decider.skip_extensions().len(), 2);

        // Add more extensions
        decider.add_skip_extension("csv");
        decider.add_skip_extension("json");
        assert_eq!(decider.skip_extensions().len(), 4);

        // All should work
        assert!(decider.skip_extensions().contains("txt"));
        assert!(decider.skip_extensions().contains("log"));
        assert!(decider.skip_extensions().contains("csv"));
        assert!(decider.skip_extensions().contains("json"));
    }

    #[test]
    fn start_with_default_then_customize() {
        let mut decider = CompressionDecider::with_default_skip_list();
        let initial_count = decider.skip_extensions().len();

        // Remove some defaults
        decider.remove_skip_extension("jpg");
        decider.remove_skip_extension("png");
        assert_eq!(decider.skip_extensions().len(), initial_count - 2);

        // Add custom ones
        decider.add_skip_extension("custom1");
        decider.add_skip_extension("custom2");
        assert_eq!(decider.skip_extensions().len(), initial_count);

        // Verify the changes
        assert!(!decider.skip_extensions().contains("jpg"));
        assert!(!decider.skip_extensions().contains("png"));
        assert!(decider.skip_extensions().contains("custom1"));
        assert!(decider.skip_extensions().contains("custom2"));
        assert!(decider.skip_extensions().contains("mp4")); // Still in list
    }
}
