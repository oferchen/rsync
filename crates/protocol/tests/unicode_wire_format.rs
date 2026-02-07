//! Comprehensive tests for Unicode filename handling in wire format encoding/decoding.
//!
//! These tests verify that Unicode filenames are correctly encoded and decoded
//! through the rsync wire protocol, maintaining byte-exact fidelity across
//! read/write roundtrips.
//!
//! # Test Coverage
//!
//! - Basic UTF-8 filenames (Latin, Cyrillic, Greek)
//! - CJK characters (Chinese, Japanese, Korean)
//! - Emoji in filenames (including complex sequences)
//! - Right-to-left scripts (Arabic, Hebrew)
//! - Combining characters and diacritics
//! - Zero-width characters
//! - Normalization forms (NFC, NFD)
//! - Invalid UTF-8 sequences (Unix-only)
//! - Mixed scripts in single filename
//! - Unicode directory names
//! - Very long Unicode filenames
//! - Prefix compression with Unicode paths

use std::io::Cursor;
use std::path::PathBuf;

use protocol::ProtocolVersion;
use protocol::flist::{FileEntry, FileListReader, FileListWriter};

// ============================================================================
// Test Helpers
// ============================================================================

/// Roundtrip test: encode then decode a single entry.
fn roundtrip_entry(entry: &FileEntry, protocol: ProtocolVersion) -> FileEntry {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    writer.write_entry(&mut buf, entry).expect("write failed");
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol);
    reader
        .read_entry(&mut cursor)
        .expect("read failed")
        .expect("no entry")
}

/// Roundtrip test: encode then decode multiple entries.
fn roundtrip_entries(entries: &[FileEntry], protocol: ProtocolVersion) -> Vec<FileEntry> {
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(protocol);
    for entry in entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol);
    let mut decoded = Vec::new();
    while let Some(entry) = reader.read_entry(&mut cursor).expect("read failed") {
        decoded.push(entry);
    }
    decoded
}

/// Creates a test file entry with the given name.
fn test_file(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_file(PathBuf::from(name), 1024, 0o644);
    entry.set_mtime(1700000000, 0);
    entry
}

/// Creates a test directory entry with the given name.
fn test_dir(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_directory(PathBuf::from(name), 0o755);
    entry.set_mtime(1700000000, 0);
    entry
}

/// Creates a test symlink entry with the given name and target.
fn test_symlink(name: &str, target: &str) -> FileEntry {
    let mut entry = FileEntry::new_symlink(PathBuf::from(name), PathBuf::from(target));
    entry.set_mtime(1700000000, 0);
    entry
}

// ============================================================================
// 1. Basic UTF-8 Filenames (Latin Extended)
// ============================================================================

/// Tests Latin extended characters (accented letters).
#[test]
fn wire_format_latin_extended() {
    let names = [
        "caf\u{00e9}.txt",            // cafe with e-acute
        "se\u{00f1}or.txt",           // senor with n-tilde
        "na\u{00ef}ve.txt",           // naive with i-diaeresis
        "r\u{00e9}sum\u{00e9}.txt",   // resume with accents
        "\u{00fc}ber.txt",            // uber with u-umlaut
        "fa\u{00e7}ade.txt",          // facade with c-cedilla
        "\u{00e0}_la_carte.txt",      // a with grave accent
        "\u{00f8}re.txt",             // ore with o-slash (Danish)
        "\u{00e5}ngstr\u{00f6}m.txt", // angstrom with Swedish characters
    ];

    for protocol in [
        ProtocolVersion::V28,
        ProtocolVersion::V30,
        ProtocolVersion::NEWEST,
    ] {
        for name in &names {
            let entry = test_file(name);
            let decoded = roundtrip_entry(&entry, protocol);
            assert_eq!(
                decoded.name(),
                *name,
                "Mismatch for {name} with {protocol:?}"
            );
        }
    }
}

/// Tests Cyrillic characters.
#[test]
fn wire_format_cyrillic() {
    let names = [
        "\u{041f}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442}.txt", // "Privet" (Hello) in Russian
        "\u{041c}\u{043e}\u{0441}\u{043a}\u{0432}\u{0430}.txt", // "Moskva" (Moscow)
        "\u{0410}\u{0411}\u{0412}\u{0413}\u{0414}.txt",         // Russian alphabet letters
        "\u{0430}\u{0431}\u{0432}\u{0433}\u{0434}.txt",         // Russian lowercase
        "test_\u{0440}\u{0443}\u{0441}\u{0441}\u{043a}\u{0438}\u{0439}_test.txt", // Mixed
    ];

    for protocol in [
        ProtocolVersion::V28,
        ProtocolVersion::V30,
        ProtocolVersion::NEWEST,
    ] {
        for name in &names {
            let entry = test_file(name);
            let decoded = roundtrip_entry(&entry, protocol);
            assert_eq!(decoded.name(), *name);
        }
    }
}

/// Tests Greek characters.
#[test]
fn wire_format_greek() {
    let names = [
        "\u{03b1}\u{03b2}\u{03b3}\u{03b4}.txt", // alpha, beta, gamma, delta
        "\u{0391}\u{0392}\u{0393}\u{0394}.txt", // uppercase Greek
        "\u{03c0}.txt",                         // pi
        "\u{03a9}mega.txt",                     // Omega
        "\u{039b}\u{03bf}\u{03b3}\u{03bf}\u{03c2}.txt", // "Logos"
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

// ============================================================================
// 2. CJK Characters
// ============================================================================

/// Tests Japanese filenames (Hiragana, Katakana, Kanji).
#[test]
fn wire_format_japanese() {
    let names = [
        "\u{65e5}\u{672c}\u{8a9e}.txt",                 // "Japanese" in kanji
        "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}.txt", // "Hello" in hiragana
        "\u{30b3}\u{30f3}\u{30d4}\u{30e5}\u{30fc}\u{30bf}.txt", // "Computer" in katakana
        "\u{6771}\u{4eac}.txt",                         // "Tokyo" in kanji
        "\u{5bcc}\u{58eb}\u{5c71}.txt",                 // "Mt. Fuji" in kanji
        "\u{3042}\u{3044}\u{3046}\u{3048}\u{304a}.txt", // aiueo in hiragana
        "test_\u{65e5}\u{672c}\u{8a9e}_test.txt",       // mixed with ASCII
    ];

    for protocol in [
        ProtocolVersion::V28,
        ProtocolVersion::V30,
        ProtocolVersion::NEWEST,
    ] {
        for name in &names {
            let entry = test_file(name);
            let decoded = roundtrip_entry(&entry, protocol);
            assert_eq!(decoded.name(), *name);
        }
    }
}

/// Tests Chinese filenames (Simplified and Traditional).
#[test]
fn wire_format_chinese() {
    let names = [
        "\u{4e2d}\u{6587}.txt",         // "Chinese" in Chinese
        "\u{4f60}\u{597d}.txt",         // "Hello" in Chinese
        "\u{5317}\u{4eac}.txt",         // "Beijing" in Chinese
        "\u{4e0a}\u{6d77}.txt",         // "Shanghai" in Chinese
        "\u{53f0}\u{5317}.txt",         // "Taipei" in Traditional Chinese
        "\u{7e41}\u{9ad4}\u{5b57}.txt", // "Traditional characters"
        "\u{7b80}\u{4f53}\u{5b57}.txt", // "Simplified characters"
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests Korean filenames (Hangul).
#[test]
fn wire_format_korean() {
    let names = [
        "\u{d55c}\u{ad6d}\u{c5b4}.txt",                 // "Korean" in Korean
        "\u{c548}\u{b155}\u{d558}\u{c138}\u{c694}.txt", // "Hello" in Korean
        "\u{c11c}\u{c6b8}.txt",                         // "Seoul" in Korean
        "\u{bd80}\u{c0b0}.txt",                         // "Busan" in Korean
        "\u{ac00}\u{b098}\u{b2e4}.txt",                 // "ga na da" in Korean
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests CJK nested directory structure.
#[test]
fn wire_format_cjk_nested_directories() {
    let entries = [
        test_dir("\u{65e5}\u{672c}"),                  // Japanese dir
        test_dir("\u{65e5}\u{672c}/\u{4e2d}\u{56fd}"), // Chinese subdir
        test_file("\u{65e5}\u{672c}/\u{4e2d}\u{56fd}/\u{d55c}\u{ad6d}.txt"), // Korean file
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);
    assert_eq!(decoded.len(), 3);
    assert_eq!(decoded[0].name(), "\u{65e5}\u{672c}");
    assert_eq!(decoded[1].name(), "\u{65e5}\u{672c}/\u{4e2d}\u{56fd}");
    assert_eq!(
        decoded[2].name(),
        "\u{65e5}\u{672c}/\u{4e2d}\u{56fd}/\u{d55c}\u{ad6d}.txt"
    );
}

// ============================================================================
// 3. Emoji Characters
// ============================================================================

/// Tests basic emoji in filenames.
#[test]
fn wire_format_emoji_basic() {
    let names = [
        "\u{1f389}.txt",           // Party popper
        "\u{1f680}.txt",           // Rocket
        "\u{1f4c1}.txt",           // File folder
        "\u{1f600}.txt",           // Grinning face
        "\u{1f4bb}.txt",           // Laptop
        "\u{2705}.txt",            // Check mark
        "test_\u{1f389}_file.txt", // Mixed ASCII and emoji
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests complex emoji sequences (ZWJ, skin tones, flags).
#[test]
fn wire_format_emoji_complex_sequences() {
    let names = [
        // Emoji with variation selector
        "\u{2764}\u{fe0f}.txt", // Red heart with variation selector
        // Emoji with skin tone modifier
        "\u{1f44d}\u{1f3fb}.txt", // Thumbs up with light skin tone
        // ZWJ sequences (family emoji)
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}.txt", // Family: man, woman, girl
        // Professional emoji
        "\u{1f469}\u{200d}\u{1f4bb}.txt", // Woman technologist
        // Flag emoji (regional indicators)
        "\u{1f1ef}\u{1f1f5}.txt", // Japan flag
        "\u{1f1fa}\u{1f1f8}.txt", // US flag
        // Multiple emoji in sequence
        "\u{1f600}\u{1f389}\u{1f680}.txt", // Multiple emoji together
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

// ============================================================================
// 4. Right-to-Left Scripts
// ============================================================================

/// Tests Arabic filenames.
#[test]
fn wire_format_arabic() {
    let names = [
        "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}.txt", // "Hello" in Arabic
        "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064a}\u{0629}.txt", // "Arabic"
        "\u{0627}\u{0644}\u{0642}\u{0627}\u{0647}\u{0631}\u{0629}.txt", // "Cairo"
        "\u{0661}\u{0662}\u{0663}.txt",                 // Arabic numerals
        "test_\u{0639}\u{0631}\u{0628}\u{064a}_file.txt", // Mixed
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests Hebrew filenames.
#[test]
fn wire_format_hebrew() {
    let names = [
        "\u{05e9}\u{05dc}\u{05d5}\u{05dd}.txt",         // "Shalom"
        "\u{05e2}\u{05d1}\u{05e8}\u{05d9}\u{05ea}.txt", // "Hebrew"
        "\u{05d9}\u{05e8}\u{05d5}\u{05e9}\u{05dc}\u{05d9}\u{05dd}.txt", // "Jerusalem"
        "\u{05e9}\u{05b8}\u{05dc}\u{05d5}\u{05b9}\u{05dd}.txt", // "Shalom" with vowels
        "test_\u{05e2}\u{05d1}\u{05e8}\u{05d9}_file.txt", // Mixed
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests bidirectional text (mixed LTR and RTL).
#[test]
fn wire_format_bidirectional_text() {
    let names = [
        "\u{05e9}\u{05dc}\u{05d5}\u{05dd}_hello.txt", // Hebrew + ASCII
        "hello_\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}.txt", // ASCII + Arabic
        "\u{05e2}\u{05d1}\u{05e8}\u{05d9}_test_\u{0639}\u{0631}\u{0628}\u{064a}.txt", // Hebrew + ASCII + Arabic
        // With explicit bidi control characters
        "\u{202a}LTR\u{202c}.txt", // LTR embedding
        "\u{202b}RTL\u{202c}.txt", // RTL embedding
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

// ============================================================================
// 5. Combining Characters and Diacritics
// ============================================================================

/// Tests combining characters (diacritical marks).
#[test]
fn wire_format_combining_characters() {
    let names = [
        // e followed by combining acute accent (U+0301)
        "cafe\u{0301}.txt",
        // a followed by combining ring above (U+030A)
        "a\u{030a}ngstrom.txt",
        // o followed by combining diaeresis (U+0308)
        "co\u{0308}operate.txt",
        // n followed by combining tilde (U+0303)
        "man\u{0303}ana.txt",
        // Multiple combining marks on same character
        "a\u{0301}\u{0308}.txt",
        // Vietnamese with multiple diacritics
        "Vie\u{0323}\u{0302}t.txt",
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests precomposed vs decomposed forms (NFC vs NFD).
#[test]
fn wire_format_normalization_forms() {
    // Precomposed: single code point for accented character
    let precomposed = "caf\u{00e9}.txt"; // U+00E9 = precomposed e-acute

    // Decomposed: base character + combining mark
    let decomposed = "cafe\u{0301}.txt"; // U+0065 U+0301 = e + combining acute

    // Both should roundtrip exactly as given (no normalization)
    let entry_pre = test_file(precomposed);
    let decoded_pre = roundtrip_entry(&entry_pre, ProtocolVersion::NEWEST);
    assert_eq!(decoded_pre.name(), precomposed);

    let entry_dec = test_file(decomposed);
    let decoded_dec = roundtrip_entry(&entry_dec, ProtocolVersion::NEWEST);
    assert_eq!(decoded_dec.name(), decomposed);

    // They should remain distinct
    assert_ne!(precomposed, decomposed);
}

/// Tests extended grapheme clusters.
#[test]
fn wire_format_extended_grapheme_clusters() {
    let names = [
        // Devanagari conjuncts
        "\u{0915}\u{094d}\u{0937}.txt", // ksha conjunct
        // Thai with tone marks
        "\u{0e01}\u{0e48}.txt",
        // Tibetan stacking
        "\u{0f40}\u{0f74}.txt",
        // Korean jamo (individual components)
        "\u{1100}\u{1161}\u{11a8}.txt",
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

// ============================================================================
// 6. Zero-width Characters
// ============================================================================

/// Tests zero-width characters in filenames.
#[test]
fn wire_format_zero_width_characters() {
    let names = [
        "test\u{200b}file.txt",         // Zero-width space
        "test\u{200c}file.txt",         // Zero-width non-joiner
        "test\u{200d}file.txt",         // Zero-width joiner
        "test\u{feff}file.txt",         // Byte order mark / zero-width no-break space
        "\u{200b}start.txt",            // Zero-width at start
        "end\u{200b}.txt",              // Zero-width at end
        "\u{200b}\u{200c}\u{200d}.txt", // Multiple zero-width characters
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

// ============================================================================
// 7. Invalid UTF-8 Sequences (Unix-specific)
// ============================================================================

/// Tests non-UTF8 byte sequences in filenames (Unix-specific).
#[cfg(unix)]
#[test]
fn wire_format_non_utf8_sequences() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let byte_sequences: &[&[u8]] = &[
        b"file\x80invalid.txt",      // Invalid continuation byte
        b"file\xff\xfe.txt",         // Invalid bytes
        b"file\xc0\xaf.txt",         // Overlong encoding
        b"file\xe0\x80\xaf.txt",     // Another overlong
        b"file\xf0\x80\x80\xaf.txt", // 4-byte overlong
    ];

    for bytes in byte_sequences {
        let path = PathBuf::from(OsStr::from_bytes(bytes));
        let mut entry = FileEntry::new_file(path.clone(), 1024, 0o644);
        entry.set_mtime(1700000000, 0);

        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.path(), &path);
    }
}

/// Tests high bytes (0x80-0xFF) in filenames.
#[cfg(unix)]
#[test]
fn wire_format_high_bytes() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let byte_sequences: &[&[u8]] = &[
        b"file\x80.txt",
        b"file\x90.txt",
        b"file\xa0.txt",
        b"file\xb0.txt",
        b"file\xc0.txt",
        b"file\xd0.txt",
        b"file\xe0.txt",
        b"file\xf0.txt",
        b"file\xff.txt",
    ];

    for bytes in byte_sequences {
        let path = PathBuf::from(OsStr::from_bytes(bytes));
        let mut entry = FileEntry::new_file(path.clone(), 1024, 0o644);
        entry.set_mtime(1700000000, 0);

        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.path(), &path);
    }
}

// ============================================================================
// 8. Mixed Scripts in Single Filename
// ============================================================================

/// Tests filenames with multiple scripts.
#[test]
fn wire_format_mixed_scripts() {
    let names = [
        // ASCII + CJK
        "hello_\u{4e16}\u{754c}.txt",
        // Latin + Greek
        "alpha_\u{03b1}_beta_\u{03b2}.txt",
        // Latin + Cyrillic
        "english_\u{0440}\u{0443}\u{0441}\u{0441}\u{043a}\u{0438}\u{0439}.txt",
        // CJK + Latin + Emoji
        "\u{65e5}\u{672c}_Japan_\u{1f1ef}\u{1f1f5}.txt",
        // Arabic + Hebrew (both RTL)
        "\u{0639}\u{0631}\u{0628}\u{064a}_\u{05e2}\u{05d1}\u{05e8}\u{05d9}.txt",
        // Latin + Thai
        "test_\u{0e17}\u{0e14}\u{0e2a}\u{0e2d}\u{0e1a}.txt",
        // All major script families in one name
        "A\u{00e9}\u{03b1}\u{0430}\u{4e2d}\u{05d0}\u{0639}\u{1f600}.txt",
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests mixed-script nested paths.
#[test]
fn wire_format_mixed_scripts_nested_paths() {
    let latin_dir = "documents";
    let japanese_dir = "\u{30c9}\u{30ad}\u{30e5}\u{30e1}\u{30f3}\u{30c8}"; // "Document" in Japanese
    let arabic_file = "\u{0645}\u{0644}\u{0641}.txt"; // "File" in Arabic

    let entries = [
        test_dir(latin_dir),
        test_dir(&format!("{latin_dir}/{japanese_dir}")),
        test_file(&format!("{latin_dir}/{japanese_dir}/{arabic_file}")),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);
    assert_eq!(decoded.len(), 3);
    assert_eq!(
        decoded[2].name(),
        format!("{latin_dir}/{japanese_dir}/{arabic_file}")
    );
}

// ============================================================================
// 9. Unicode Directory Names
// ============================================================================

/// Tests Unicode directory entries.
#[test]
fn wire_format_unicode_directories() {
    let dirs = [
        "\u{6587}\u{4ef6}\u{5939}",         // "Folder" in Chinese
        "\u{0444}\u{0430}\u{0439}\u{043b}", // "File" in Russian
        "\u{1f4c1}_folder",                 // Emoji folder
        "\u{03b1}\u{03b2}\u{03b3}",         // Greek letters
    ];

    for dir_name in &dirs {
        let entry = test_dir(dir_name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *dir_name);
        assert!(decoded.is_dir());
    }
}

/// Tests deeply nested Unicode directory structure.
#[test]
fn wire_format_deeply_nested_unicode() {
    let entries = [
        test_dir("\u{4e2d}\u{6587}"),
        test_dir("\u{4e2d}\u{6587}/\u{65e5}\u{672c}\u{8a9e}"),
        test_dir("\u{4e2d}\u{6587}/\u{65e5}\u{672c}\u{8a9e}/\u{d55c}\u{ad6d}\u{c5b4}"),
        test_file("\u{4e2d}\u{6587}/\u{65e5}\u{672c}\u{8a9e}/\u{d55c}\u{ad6d}\u{c5b4}/file.txt"),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);
    assert_eq!(decoded.len(), 4);

    // Verify each path level
    assert_eq!(decoded[0].name(), "\u{4e2d}\u{6587}");
    assert_eq!(
        decoded[3].name(),
        "\u{4e2d}\u{6587}/\u{65e5}\u{672c}\u{8a9e}/\u{d55c}\u{ad6d}\u{c5b4}/file.txt"
    );
}

// ============================================================================
// 10. Very Long Unicode Filenames
// ============================================================================

/// Tests maximum-length UTF-8 filenames.
#[test]
fn wire_format_maximum_length_unicode() {
    // Using 3-byte CJK characters, 85 characters is about 255 bytes
    let cjk_char = "\u{4e2d}";
    let long_name = format!("{}.txt", cjk_char.repeat(80)); // 240 bytes + 4 = 244 bytes

    let entry = test_file(&long_name);
    let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
    assert_eq!(decoded.name(), long_name);
}

/// Tests long paths with Unicode components.
#[test]
fn wire_format_long_unicode_paths() {
    // Create a path with multiple long Unicode directory components
    let dir1 = "\u{65e5}\u{672c}\u{8a9e}".repeat(10); // ~90 bytes
    let dir2 = "\u{4e2d}\u{6587}".repeat(10); // ~60 bytes
    let file = "\u{d55c}\u{ad6d}\u{c5b4}".repeat(5); // ~45 bytes

    let path = format!("{dir1}/{dir2}/{file}.txt");
    let entry = test_file(&path);
    let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
    assert_eq!(decoded.name(), path);
}

// ============================================================================
// 11. Prefix Compression with Unicode Paths
// ============================================================================

/// Tests prefix compression with similar Unicode paths.
#[test]
fn wire_format_prefix_compression_unicode() {
    // Files with common Unicode prefix should benefit from compression
    let prefix = "\u{65e5}\u{672c}/\u{6771}\u{4eac}/";
    let entries: Vec<_> = (0..10)
        .map(|i| test_file(&format!("{prefix}file_{i:03}.txt")))
        .collect();

    // Encode with compression
    let mut buf = Vec::new();
    let mut writer = FileListWriter::new(ProtocolVersion::NEWEST);
    for entry in &entries {
        writer.write_entry(&mut buf, entry).expect("write failed");
    }
    writer.write_end(&mut buf, None).expect("write end failed");

    // Verify compression is effective
    // Without compression: ~35 bytes per entry * 10 = 350 bytes minimum
    // With compression: first entry full, subsequent entries much smaller
    assert!(
        buf.len() < 300,
        "Expected better compression, got {} bytes",
        buf.len()
    );

    // Verify roundtrip
    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);
    assert_eq!(decoded.len(), entries.len());
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(decoded[i].name(), entry.name());
    }
}

/// Tests prefix compression with CJK/ASCII mixed paths.
#[test]
fn wire_format_prefix_compression_mixed() {
    let entries = [
        test_file("project/src/\u{6a21}\u{5757}/file1.rs"),
        test_file("project/src/\u{6a21}\u{5757}/file2.rs"),
        test_file("project/src/\u{6a21}\u{5757}/file3.rs"),
        test_file("project/src/\u{6a21}\u{5757}_test/test1.rs"),
        test_file("project/src/\u{6a21}\u{5757}_test/test2.rs"),
    ];

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);
    assert_eq!(decoded.len(), entries.len());
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(decoded[i].name(), entry.name());
    }
}

// ============================================================================
// 12. Protocol Version Compatibility
// ============================================================================

/// Tests Unicode across all supported protocol versions.
#[test]
fn wire_format_unicode_all_protocol_versions() {
    let test_names = [
        "ascii.txt",
        "caf\u{00e9}.txt",
        "\u{65e5}\u{672c}\u{8a9e}.txt",
        "\u{1f600}.txt",
        "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}.txt",
        "a\u{0301}bc.txt",
    ];

    let protocols = [
        ProtocolVersion::V28,
        ProtocolVersion::V29,
        ProtocolVersion::V30,
        ProtocolVersion::V31,
        ProtocolVersion::NEWEST,
    ];

    for protocol in protocols {
        for name in &test_names {
            let entry = test_file(name);
            let decoded = roundtrip_entry(&entry, protocol);
            assert_eq!(decoded.name(), *name, "Failed for {name} with {protocol:?}");
        }
    }
}

// ============================================================================
// 13. Supplementary Multilingual Plane (SMP) Characters
// ============================================================================

/// Tests characters from the Supplementary Multilingual Plane (U+10000+).
#[test]
fn wire_format_smp_characters() {
    let names = [
        "\u{10000}.txt", // Linear B Syllable B008 A
        "\u{10300}.txt", // Old Italic Letter A
        "\u{10400}.txt", // Deseret Capital Letter Long I
        "\u{1d400}.txt", // Mathematical Bold Capital A
        "\u{20000}.txt", // CJK Extension B character
        "\u{1f300}.txt", // Cyclone (weather emoji)
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

// ============================================================================
// 14. Unicode Symlinks
// ============================================================================

/// Tests symlinks with Unicode names and targets.
#[test]
fn wire_format_unicode_symlinks() {
    let protocol = ProtocolVersion::NEWEST;

    // Symlink with Unicode name
    let symlink1 = test_symlink("\u{94fe}\u{63a5}.lnk", "target.txt");
    let mut writer = FileListWriter::new(protocol).with_preserve_links(true);
    let mut buf = Vec::new();
    writer
        .write_entry(&mut buf, &symlink1)
        .expect("write failed");
    writer.write_end(&mut buf, None).expect("write end failed");

    let mut cursor = Cursor::new(&buf);
    let mut reader = FileListReader::new(protocol).with_preserve_links(true);
    let decoded = reader
        .read_entry(&mut cursor)
        .expect("read failed")
        .expect("no entry");
    assert_eq!(decoded.name(), "\u{94fe}\u{63a5}.lnk");

    // Symlink with Unicode target
    let symlink2 = test_symlink("link.txt", "\u{76ee}\u{6807}/\u{6587}\u{4ef6}.txt");
    let mut buf2 = Vec::new();
    let mut writer2 = FileListWriter::new(protocol).with_preserve_links(true);
    writer2
        .write_entry(&mut buf2, &symlink2)
        .expect("write failed");
    writer2
        .write_end(&mut buf2, None)
        .expect("write end failed");

    let mut cursor2 = Cursor::new(&buf2);
    let mut reader2 = FileListReader::new(protocol).with_preserve_links(true);
    let decoded2 = reader2
        .read_entry(&mut cursor2)
        .expect("read failed")
        .expect("no entry");
    assert_eq!(
        decoded2
            .link_target()
            .expect("should have target")
            .to_str()
            .unwrap(),
        "\u{76ee}\u{6807}/\u{6587}\u{4ef6}.txt"
    );
}

// ============================================================================
// 15. Unicode Stress Tests
// ============================================================================

/// Stress test: large number of Unicode entries.
#[test]
fn wire_format_unicode_stress_many_entries() {
    let prefixes = [
        "\u{65e5}\u{672c}", // Japanese
        "\u{4e2d}\u{6587}", // Chinese
        "\u{d55c}\u{ad6d}", // Korean
        "\u{0440}\u{0443}", // Russian
        "\u{03b1}\u{03b2}", // Greek
    ];

    let entries: Vec<_> = (0..1000)
        .map(|i| {
            let prefix = prefixes[i % prefixes.len()];
            test_file(&format!("{prefix}_file_{i:04}.txt"))
        })
        .collect();

    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);
    assert_eq!(decoded.len(), 1000);

    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(decoded[i].name(), entry.name());
    }
}

/// Stress test: maximum character diversity.
#[test]
fn wire_format_unicode_stress_diversity() {
    // One file for each major Unicode block
    let diverse_names = [
        "latin_\u{00e9}\u{00f1}.txt",
        "cyrillic_\u{0430}\u{0431}.txt",
        "greek_\u{03b1}\u{03b2}.txt",
        "arabic_\u{0639}\u{0631}.txt",
        "hebrew_\u{05d0}\u{05d1}.txt",
        "devanagari_\u{0905}\u{0906}.txt",
        "thai_\u{0e01}\u{0e02}.txt",
        "cjk_\u{4e2d}\u{6587}.txt",
        "hiragana_\u{3042}\u{3044}.txt",
        "katakana_\u{30a2}\u{30a4}.txt",
        "hangul_\u{ac00}\u{ac01}.txt",
        "emoji_\u{1f600}\u{1f389}.txt",
        "math_\u{221e}\u{2211}.txt",
        "currency_\u{20ac}\u{00a3}.txt",
        "arrows_\u{2190}\u{2191}.txt",
        "box_\u{2500}\u{2501}.txt",
        "braille_\u{2800}\u{2801}.txt",
    ];

    let entries: Vec<_> = diverse_names.iter().map(|name| test_file(name)).collect();
    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), diverse_names.len());
    for (i, name) in diverse_names.iter().enumerate() {
        assert_eq!(decoded[i].name(), *name);
    }
}

// ============================================================================
// 16. Edge Cases
// ============================================================================

/// Tests Unicode edge cases.
#[test]
fn wire_format_unicode_edge_cases() {
    let names = [
        // Single character filenames
        "\u{4e2d}.txt",  // Single CJK
        "\u{1f600}.txt", // Single emoji
        "\u{0301}.txt",  // Just combining mark
        // Repeated characters
        "\u{4e2d}\u{4e2d}\u{4e2d}.txt",
        // Unicode whitespace
        "\u{3000}space\u{3000}.txt", // Ideographic space
        "\u{2003}em\u{2003}.txt",    // Em space
        // Unicode punctuation
        "\u{3001}comma\u{3002}.txt", // Ideographic comma and period
        // Private Use Area
        "\u{e000}.txt",
        // Replacement character
        "\u{fffd}.txt",
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests visually similar but distinct Unicode characters.
#[test]
fn wire_format_visually_similar_distinct() {
    let names = [
        "a.txt",        // Latin lowercase a
        "\u{0430}.txt", // Cyrillic lowercase a
        "\u{03b1}.txt", // Greek lowercase alpha
        "A.txt",        // Latin uppercase A
        "\u{0410}.txt", // Cyrillic uppercase A
        "o.txt",        // Latin lowercase o
        "\u{03bf}.txt", // Greek lowercase omicron
        "\u{043e}.txt", // Cyrillic lowercase o
    ];

    let entries: Vec<_> = names.iter().map(|name| test_file(name)).collect();
    let decoded = roundtrip_entries(&entries, ProtocolVersion::NEWEST);

    assert_eq!(decoded.len(), names.len());
    for (i, name) in names.iter().enumerate() {
        assert_eq!(decoded[i].name(), *name);
    }

    // Verify they're all distinct
    for i in 0..decoded.len() {
        for j in (i + 1)..decoded.len() {
            assert_ne!(decoded[i].name(), decoded[j].name());
        }
    }
}

// ============================================================================
// 17. Mathematical and Technical Symbols
// ============================================================================

/// Tests mathematical and technical symbols.
#[test]
fn wire_format_mathematical_symbols() {
    let names = [
        "\u{221e}.txt",            // Infinity
        "\u{2211}.txt",            // Summation
        "\u{222b}.txt",            // Integral
        "\u{2202}.txt",            // Partial differential
        "\u{03c0}.txt",            // Pi
        "\u{221a}.txt",            // Square root
        "x\u{00b2}+y\u{00b2}.txt", // x squared + y squared
        "\u{2261}.txt",            // Identical to
        "\u{2248}.txt",            // Almost equal to
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}

/// Tests currency symbols.
#[test]
fn wire_format_currency_symbols() {
    let names = [
        "\u{20ac}100.txt",               // Euro
        "\u{00a3}100.txt",               // Pound
        "\u{00a5}100.txt",               // Yen
        "\u{20b9}100.txt",               // Indian Rupee
        "\u{20bf}100.txt",               // Bitcoin
        "price_$_\u{20ac}_\u{00a3}.txt", // Multiple currencies
    ];

    for name in &names {
        let entry = test_file(name);
        let decoded = roundtrip_entry(&entry, ProtocolVersion::NEWEST);
        assert_eq!(decoded.name(), *name);
    }
}
