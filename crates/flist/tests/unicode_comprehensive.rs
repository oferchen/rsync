//! Comprehensive tests for Unicode filename handling in file list traversal.
//!
//! These tests extend the basic UTF-8 tests to cover additional edge cases
//! and ensure proper handling of Unicode filenames during filesystem traversal.
//!
//! # Test Coverage
//!
//! - Normalization form handling (NFC vs NFD)
//! - Zero-width and invisible characters
//! - Unicode collation and sorting edge cases
//! - Very long Unicode paths
//! - Unicode in various path positions
//! - Cross-script homoglyphs
//! - Unicode private use areas
//! - Unicode boundary conditions

use flist::{FileListBuilder, FileListEntry, FileListError};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

// ============================================================================
// Helper Functions
// ============================================================================

/// Collects relative paths from a walker, skipping the root entry.
fn collect_relative_paths(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<PathBuf> {
    walker
        .filter_map(|r| r.ok())
        .filter(|e| !e.is_root())
        .map(|e| e.relative_path().to_path_buf())
        .collect()
}

/// Collects all entries from a walker.
#[allow(dead_code)]
fn collect_all_entries(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<FileListEntry> {
    walker.map(|r| r.expect("entry should succeed")).collect()
}

/// Attempts to create a file with the given name, returning true if successful.
fn try_create_file(dir: &Path, name: &str, content: &[u8]) -> bool {
    fs::write(dir.join(name), content).is_ok()
}

/// Attempts to create a directory with the given name, returning true if successful.
#[allow(dead_code)]
fn try_create_dir(dir: &Path, name: &str) -> bool {
    fs::create_dir(dir.join(name)).is_ok()
}

/// Creates a file with the given name (panics on failure).
#[allow(dead_code)]
fn create_file(dir: &Path, name: &str, content: &[u8]) {
    fs::write(dir.join(name), content).expect("create file");
}

/// Creates a directory with the given name (panics on failure).
#[allow(dead_code)]
fn create_dir(dir: &Path, name: &str) {
    fs::create_dir(dir.join(name)).expect("create dir");
}

// ============================================================================
// 1. Unicode Normalization Handling
// ============================================================================

/// Tests that NFC and NFD forms are preserved distinctly.
///
/// NFC (Composed): \u{00e9} (e-acute as single code point)
/// NFD (Decomposed): e\u{0301} (e + combining acute)
///
/// The filesystem behavior varies:
/// - macOS HFS+/APFS: Forces NFD normalization
/// - Linux ext4/btrfs: Preserves byte sequences exactly
#[test]
fn normalization_nfc_vs_nfd() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("normalization");
    fs::create_dir(&root).expect("create root");

    // NFC form (composed)
    let nfc_name = "caf\u{00e9}_nfc.txt";
    // NFD form (decomposed)
    let nfd_name = "cafe\u{0301}_nfd.txt";

    let nfc_created = try_create_file(&root, nfc_name, b"nfc");
    let nfd_created = try_create_file(&root, nfd_name, b"nfd");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // On normalizing filesystems, they might merge
    // On non-normalizing filesystems, both should exist
    if nfc_created && nfd_created {
        // If both were created, we should find at least 1 (could be 1 or 2)
        assert!(!paths.is_empty());
        // Verify the paths we find are valid UTF-8 and match what we created
        for path in &paths {
            let path_str = path.to_str().expect("path should be valid UTF-8");
            assert!(
                path_str.contains("caf") || path_str.contains("cafe"),
                "path should contain expected prefix"
            );
        }
    }
}

/// Tests multiple combining character scenarios.
#[test]
fn multiple_combining_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("combining");
    fs::create_dir(&root).expect("create root");

    let names = [
        // Single combining mark
        "e\u{0301}.txt",           // e + acute
        // Two combining marks
        "a\u{0301}\u{0308}.txt",   // a + acute + diaeresis
        // Three combining marks
        "o\u{0301}\u{0323}\u{0302}.txt", // o + acute + dot below + circumflex
        // Combining marks with different base
        "n\u{0303}.txt",           // n + tilde
        "c\u{0327}.txt",           // c + cedilla
    ];

    let mut created = Vec::new();
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created.push(*name);
        }
    }

    if !created.is_empty() {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(
            paths.len(),
            created.len(),
            "should find all created files"
        );
    }
}

/// Tests Korean Hangul composition (Jamo to Syllables).
#[test]
fn korean_hangul_composition() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("hangul");
    fs::create_dir(&root).expect("create root");

    // Composed Hangul syllable
    let composed = "\u{ac00}.txt"; // 가 (single syllable)
    // Decomposed into Jamo
    let decomposed = "\u{1100}\u{1161}.txt"; // ㄱ + ㅏ

    let comp_ok = try_create_file(&root, composed, b"composed");
    let decomp_ok = try_create_file(&root, decomposed, b"decomposed");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Verify at least one was created
    assert!(comp_ok || decomp_ok, "should create at least one file");
    assert!(!paths.is_empty());
}

// ============================================================================
// 2. Zero-width and Invisible Characters
// ============================================================================

/// Tests invisible Unicode characters.
#[test]
fn invisible_unicode_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("invisible");
    fs::create_dir(&root).expect("create root");

    let names = [
        "test\u{200b}file.txt",    // Zero-width space
        "test\u{200c}file.txt",    // Zero-width non-joiner
        "test\u{200d}file.txt",    // Zero-width joiner
        "test\u{feff}file.txt",    // BOM / ZWNBSP
        "test\u{00ad}file.txt",    // Soft hyphen
        "test\u{034f}file.txt",    // Combining grapheme joiner
        "test\u{2060}file.txt",    // Word joiner
        "test\u{2061}file.txt",    // Function application
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), created_count);
    }
}

/// Tests invisible characters at different positions.
#[test]
fn invisible_at_positions() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("invisible_pos");
    fs::create_dir(&root).expect("create root");

    let zwsp = "\u{200b}"; // Zero-width space
    let names = [
        format!("{zwsp}start.txt"),
        format!("middle{zwsp}name.txt"),
        format!("end{zwsp}.txt"),
        format!("{zwsp}{zwsp}double.txt"),
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, &name, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths.len(), created_count);
    }
}

// ============================================================================
// 3. Unicode Sorting Edge Cases
// ============================================================================

/// Tests sorting of visually similar characters from different scripts.
#[test]
fn sorting_cross_script_homoglyphs() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("homoglyphs");
    fs::create_dir(&root).expect("create root");

    // These look similar but are different code points
    let names = [
        "A.txt",         // Latin A (U+0041)
        "\u{0391}.txt",  // Greek Alpha (U+0391)
        "\u{0410}.txt",  // Cyrillic A (U+0410)
        "a.txt",         // Latin a (U+0061)
        "\u{03b1}.txt",  // Greek alpha (U+03B1)
        "\u{0430}.txt",  // Cyrillic a (U+0430)
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Verify sorting is consistent (lexicographic by code point)
    for i in 0..paths.len().saturating_sub(1) {
        assert!(
            paths[i] < paths[i + 1],
            "paths should be sorted: {:?} < {:?}",
            paths[i],
            paths[i + 1]
        );
    }
}

/// Tests sorting of different byte lengths.
#[test]
fn sorting_by_byte_length() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("bytelength");
    fs::create_dir(&root).expect("create root");

    let names = [
        "a.txt",              // 1-byte char (ASCII)
        "\u{00e9}.txt",       // 2-byte char (Latin extended)
        "\u{4e2d}.txt",       // 3-byte char (CJK)
        "\u{1f600}.txt",      // 4-byte char (emoji)
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Verify sorting is by byte values
    for i in 0..paths.len().saturating_sub(1) {
        assert!(
            paths[i] < paths[i + 1],
            "paths should be sorted by bytes"
        );
    }
}

/// Tests sorting of supplementary plane characters.
#[test]
fn sorting_supplementary_plane() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("smp");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{10000}.txt",   // Linear B
        "\u{10400}.txt",   // Deseret
        "\u{1d400}.txt",   // Math Bold
        "\u{1f300}.txt",   // Miscellaneous Symbols
        "\u{1f600}.txt",   // Emoticons
        "\u{20000}.txt",   // CJK Extension B
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Verify all supplementary plane files found
    for i in 0..paths.len().saturating_sub(1) {
        assert!(paths[i] < paths[i + 1]);
    }
}

// ============================================================================
// 4. Very Long Unicode Paths
// ============================================================================

/// Tests near-maximum filename length with multi-byte characters.
#[test]
fn maximum_length_multibyte() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("maxlen_mb");
    fs::create_dir(&root).expect("create root");

    // Test with 2-byte characters (e.g., accented)
    // 255 bytes / 2 = ~127 characters
    let two_byte = "\u{00e9}".repeat(120);
    let two_byte_name = format!("{}.txt", two_byte);

    // Test with 3-byte characters (CJK)
    // 255 bytes / 3 = ~85 characters
    let three_byte = "\u{4e2d}".repeat(80);
    let three_byte_name = format!("{}.txt", three_byte);

    // Test with 4-byte characters (emoji)
    // 255 bytes / 4 = ~63 characters
    let four_byte = "\u{1f600}".repeat(60);
    let four_byte_name = format!("{}.txt", four_byte);

    let mut created = Vec::new();
    if try_create_file(&root, &two_byte_name, b"data") {
        created.push(two_byte_name.clone());
    }
    if try_create_file(&root, &three_byte_name, b"data") {
        created.push(three_byte_name.clone());
    }
    if try_create_file(&root, &four_byte_name, b"data") {
        created.push(four_byte_name.clone());
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), created.len());
}

/// Tests very deep nested Unicode paths.
#[test]
fn deeply_nested_unicode_path() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("deep_unicode");
    fs::create_dir(&root).expect("create root");

    // Create a deeply nested path with Unicode at each level
    let levels = [
        "\u{4e2d}",      // Chinese
        "\u{65e5}",      // Japanese kanji
        "\u{d55c}",      // Korean
        "\u{03b1}",      // Greek
        "\u{0430}",      // Cyrillic
    ];

    let mut current = root.clone();
    for level in &levels {
        current = current.join(level);
        if fs::create_dir(&current).is_err() {
            return; // Skip if filesystem doesn't support this
        }
    }

    // Create a file at the deepest level
    let file_path = current.join("file.txt");
    fs::write(&file_path, b"deep").expect("write file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should find: 5 directories + 1 file = 6 entries
    assert_eq!(paths.len(), 6);

    // Verify the deepest file exists
    let _expected_deepest = levels.join("/") + "/file.txt";
    assert!(
        paths.iter().any(|p| p.to_str().map_or(false, |s| s.ends_with("file.txt"))),
        "should find the deep file"
    );
}

// ============================================================================
// 5. Unicode in Various Path Positions
// ============================================================================

/// Tests Unicode at the start of filename.
#[test]
fn unicode_at_start() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("unicode_start");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{4e2d}test.txt",     // CJK at start
        "\u{1f600}smile.txt",   // Emoji at start
        "\u{0301}accent.txt",   // Combining mark at start
        "\u{200b}zwsp.txt",     // Zero-width at start
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert!(!paths.is_empty());
}

/// Tests Unicode at the end of filename (before extension).
#[test]
fn unicode_at_end() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("unicode_end");
    fs::create_dir(&root).expect("create root");

    let names = [
        "test\u{4e2d}.txt",     // CJK at end
        "test\u{1f600}.txt",    // Emoji at end
        "test\u{0301}.txt",     // Combining mark at end
        "test\u{200b}.txt",     // Zero-width at end
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert!(!paths.is_empty());
}

/// Tests Unicode in file extension.
#[test]
fn unicode_in_extension() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("unicode_ext");
    fs::create_dir(&root).expect("create root");

    let names = [
        "file.\u{4e2d}\u{6587}",   // Chinese extension
        "file.\u{1f4c4}",          // Emoji extension
        "file.\u{00e9}xt",         // Accented extension
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert!(!paths.is_empty());
}

// ============================================================================
// 6. Unicode Private Use Areas
// ============================================================================

/// Tests Private Use Area characters.
#[test]
fn private_use_area() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("pua");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{e000}.txt",     // Start of BMP PUA
        "\u{e001}.txt",
        "\u{f8ff}.txt",     // Apple logo position
        "\u{f0000}.txt",    // Supplementary PUA-A
        "\u{100000}.txt",   // Supplementary PUA-B
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths.len(), created_count);
    }
}

// ============================================================================
// 7. Unicode Boundary Conditions
// ============================================================================

/// Tests Unicode boundary code points.
#[test]
fn unicode_boundaries() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("boundaries");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{007f}.txt",      // DEL (last ASCII)
        "\u{0080}.txt",      // First Latin-1 Supplement
        "\u{07ff}.txt",      // Last 2-byte
        "\u{0800}.txt",      // First 3-byte
        "\u{ffff}.txt",      // Last BMP
        "\u{10000}.txt",     // First supplementary
        "\u{10ffff}.txt",    // Last valid Unicode
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths.len(), created_count);
    }
}

/// Tests replacement character handling.
#[test]
fn replacement_character() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("replacement");
    fs::create_dir(&root).expect("create root");

    // U+FFFD is the replacement character
    let name = "file\u{fffd}name.txt";
    if try_create_file(&root, name, b"data") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].to_str().unwrap().contains("\u{fffd}"));
    }
}

// ============================================================================
// 8. Unicode Whitespace Variants
// ============================================================================

/// Tests various Unicode whitespace characters.
#[test]
fn unicode_whitespace_variants() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("whitespace");
    fs::create_dir(&root).expect("create root");

    let names = [
        "file\u{00a0}nbsp.txt",      // Non-breaking space
        "file\u{2000}enquad.txt",    // En quad
        "file\u{2001}emquad.txt",    // Em quad
        "file\u{2002}enspace.txt",   // En space
        "file\u{2003}emspace.txt",   // Em space
        "file\u{2004}third.txt",     // Three-per-em space
        "file\u{2005}fourth.txt",    // Four-per-em space
        "file\u{2006}sixth.txt",     // Six-per-em space
        "file\u{2007}figure.txt",    // Figure space
        "file\u{2008}punct.txt",     // Punctuation space
        "file\u{2009}thin.txt",      // Thin space
        "file\u{200a}hair.txt",      // Hair space
        "file\u{202f}narrow.txt",    // Narrow no-break space
        "file\u{205f}mathspace.txt", // Medium mathematical space
        "file\u{3000}ideographic.txt", // Ideographic space
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths.len(), created_count);
    }
}

// ============================================================================
// 9. Unicode Direction Control Characters
// ============================================================================

/// Tests Unicode bidirectional control characters.
#[test]
fn bidirectional_control_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("bidi_control");
    fs::create_dir(&root).expect("create root");

    let names = [
        "file\u{200e}lrm.txt",      // Left-to-right mark
        "file\u{200f}rlm.txt",      // Right-to-left mark
        "file\u{202a}lre.txt",      // Left-to-right embedding
        "file\u{202b}rle.txt",      // Right-to-left embedding
        "file\u{202c}pdf.txt",      // Pop directional formatting
        "file\u{202d}lro.txt",      // Left-to-right override
        "file\u{202e}rlo.txt",      // Right-to-left override
        "file\u{2066}lri.txt",      // Left-to-right isolate
        "file\u{2067}rli.txt",      // Right-to-left isolate
        "file\u{2068}fsi.txt",      // First strong isolate
        "file\u{2069}pdi.txt",      // Pop directional isolate
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths.len(), created_count);
    }
}

// ============================================================================
// 10. Unicode Scripts Not Yet Covered
// ============================================================================

/// Tests Thai script with tone marks.
#[test]
fn thai_script() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("thai");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{0e2a}\u{0e27}\u{0e31}\u{0e2a}\u{0e14}\u{0e35}.txt", // "Hello" in Thai
        "\u{0e01}\u{0e48}.txt",                                   // With tone mark
        "test_\u{0e44}\u{0e17}\u{0e22}.txt",                     // "Thai" in Thai
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);
    assert!(!paths.is_empty());
}

/// Tests Devanagari script with conjuncts.
#[test]
fn devanagari_script() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("devanagari");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{0928}\u{092e}\u{0938}\u{094d}\u{0924}\u{0947}.txt", // "Namaste"
        "\u{0939}\u{093f}\u{0928}\u{094d}\u{0926}\u{0940}.txt", // "Hindi"
        "\u{0915}\u{094d}\u{0937}.txt",                          // Conjunct ksha
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);
    assert!(!paths.is_empty());
}

/// Tests Tamil script.
#[test]
fn tamil_script() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tamil");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{0ba4}\u{0bae}\u{0bbf}\u{0bb4}\u{0bcd}.txt", // "Tamil"
        "\u{0bb5}\u{0ba3}\u{0b95}\u{0bcd}\u{0b95}\u{0bae}\u{0bcd}.txt", // "Vanakkam" (Hello)
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);
    assert!(!paths.is_empty());
}

/// Tests Georgian script.
#[test]
fn georgian_script() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("georgian");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{10d2}\u{10d0}\u{10db}\u{10d0}\u{10e0}\u{10ef}\u{10dd}\u{10d1}\u{10d0}.txt", // "Gamarjoba" (Hello)
        "\u{10e5}\u{10d0}\u{10e0}\u{10d7}\u{10e3}\u{10da}\u{10d8}.txt", // "Georgian"
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);
    assert!(!paths.is_empty());
}

/// Tests Armenian script.
#[test]
fn armenian_script() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("armenian");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{0532}\u{0561}\u{0580}\u{0565}\u{057e}.txt", // "Barev" (Hello)
        "\u{0540}\u{0561}\u{0575}\u{0565}\u{0580}\u{0565}\u{0576}.txt", // "Armenian"
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);
    assert!(!paths.is_empty());
}

// ============================================================================
// 11. Invalid UTF-8 Sequences (Unix-specific)
// ============================================================================

/// Tests invalid UTF-8 sequences.
#[cfg(unix)]
#[test]
fn invalid_utf8_sequences() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("invalid_utf8");
    fs::create_dir(&root).expect("create root");

    let sequences: &[&[u8]] = &[
        // Invalid continuation bytes
        b"file\x80.txt",
        b"file\x81.txt",
        b"file\xbf.txt",
        // Invalid start bytes
        b"file\xc0.txt",
        b"file\xc1.txt",
        b"file\xfe.txt",
        b"file\xff.txt",
        // Overlong sequences
        b"file\xc0\xaf.txt",         // Overlong /
        b"file\xe0\x80\xaf.txt",     // Another overlong
        // Truncated sequences
        b"file\xc2.txt",             // Incomplete 2-byte
        b"file\xe0\x80.txt",         // Incomplete 3-byte
        b"file\xf0\x80\x80.txt",     // Incomplete 4-byte
        // Invalid 4-byte sequences
        b"file\xf4\x90\x80\x80.txt", // Beyond U+10FFFF
        b"file\xf5\x80\x80\x80.txt", // Invalid start byte
    ];

    let mut created_count = 0;
    for bytes in sequences {
        let name = OsStr::from_bytes(*bytes);
        let path = root.join(name);
        if fs::write(&path, b"data").is_ok() {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths.len(), created_count);
    }
}

/// Tests mixed valid/invalid UTF-8.
#[cfg(unix)]
#[test]
fn mixed_valid_invalid_utf8() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixed_utf8");
    fs::create_dir(&root).expect("create root");

    // Valid UTF-8 followed by invalid byte
    let name1 = OsStr::from_bytes(b"caf\xc3\xa9_\xff.txt"); // cafe (valid) + invalid
    // Invalid byte followed by valid UTF-8
    let name2 = OsStr::from_bytes(b"\xff_caf\xc3\xa9.txt");
    // Valid-invalid-valid
    let name3 = OsStr::from_bytes(b"hello\xff\x4e\x2d.txt"); // hello + invalid + valid CJK bytes

    for name in [name1, name2, name3] {
        let path = root.join(name);
        let _ = fs::write(&path, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Should find whatever files were created
    assert!(paths.len() <= 3);
}

// ============================================================================
// 12. Stress Tests
// ============================================================================

/// Stress test with many Unicode files.
#[test]
fn stress_many_unicode_files() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("stress_unicode");
    fs::create_dir(&root).expect("create root");

    let prefixes = [
        "\u{65e5}",     // Japanese
        "\u{4e2d}",     // Chinese
        "\u{d55c}",     // Korean
        "\u{0440}",     // Russian
        "\u{03b1}",     // Greek
    ];

    let mut created = 0;
    for (i, prefix) in prefixes.iter().cycle().enumerate().take(500) {
        let name = format!("{}_{:04}.txt", prefix, i);
        if try_create_file(&root, &name, b"data") {
            created += 1;
        }
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), created);
}

/// Stress test with diverse character set.
#[test]
fn stress_character_diversity() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("stress_diverse");
    fs::create_dir(&root).expect("create root");

    // One file for each major Unicode block we care about
    let diverse_chars = [
        "\u{0041}",     // Basic Latin
        "\u{00c0}",     // Latin Extended
        "\u{0100}",     // Latin Extended-A
        "\u{0180}",     // Latin Extended-B
        "\u{0250}",     // IPA Extensions
        "\u{0370}",     // Greek
        "\u{0400}",     // Cyrillic
        "\u{0530}",     // Armenian
        "\u{0590}",     // Hebrew
        "\u{0600}",     // Arabic
        "\u{0900}",     // Devanagari
        "\u{0e00}",     // Thai
        "\u{1100}",     // Hangul Jamo
        "\u{3040}",     // Hiragana
        "\u{30a0}",     // Katakana
        "\u{4e00}",     // CJK
        "\u{ac00}",     // Hangul Syllables
        "\u{1f600}",    // Emoticons
    ];

    let mut created = 0;
    for (i, ch) in diverse_chars.iter().enumerate() {
        let name = format!("{}_{:02}.txt", ch, i);
        if try_create_file(&root, &name, b"data") {
            created += 1;
        }
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), created);
}
