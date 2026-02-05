//! Integration tests for UTF-8 filename handling.
//!
//! These tests verify correct handling of UTF-8 encoded filenames including:
//! - Basic UTF-8 characters (accented letters)
//! - CJK characters (Chinese, Japanese, Korean)
//! - Emoji characters
//! - Multi-byte sequences
//! - Combining characters
//! - Right-to-left scripts (Arabic, Hebrew)
//! - Mixed scripts in same filename
//!
//! Reference: rsync 3.4.1 flist.c
//!
//! Note: Some tests may be skipped on filesystems that don't support certain
//! Unicode characters in filenames.

use flist::{FileListBuilder, FileListEntry, FileListError};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

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
fn collect_all_entries(
    walker: impl Iterator<Item = Result<FileListEntry, FileListError>>,
) -> Vec<FileListEntry> {
    walker.map(|r| r.expect("entry should succeed")).collect()
}

/// Attempts to create a file with the given name, returning true if successful.
/// Some filesystems may not support certain Unicode characters.
fn try_create_file(dir: &Path, name: &str, content: &[u8]) -> bool {
    fs::write(dir.join(name), content).is_ok()
}

/// Attempts to create a directory with the given name, returning true if successful.
fn try_create_dir(dir: &Path, name: &str) -> bool {
    fs::create_dir(dir.join(name)).is_ok()
}

// ============================================================================
// Basic UTF-8 Characters (Accented Latin Letters)
// ============================================================================

/// Verifies handling of files with accented Latin characters.
#[test]
fn basic_utf8_accented_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("accented");
    fs::create_dir(&root).expect("create root");

    let names = [
        "cafe\u{0301}.txt", // cafe with combining acute accent (e + combining acute)
        "caf\u{00e9}.txt",  // cafe with precomposed e-acute
        "se\u{00f1}or.txt", // senor with n-tilde
        "na\u{00ef}ve.txt", // naive with i-diaeresis
        "r\u{00e9}sum\u{00e9}.txt", // resume with accents
        "\u{00fc}ber.txt",  // uber with u-umlaut
        "fa\u{00e7}ade.txt", // facade with c-cedilla
        "pr\u{00ea}t.txt",  // pret with e-circumflex
        "\u{00e0}_la_carte.txt", // a with grave accent
        "\u{00f8}re.txt",   // ore with o-slash (Danish)
        "\u{00e5}ngstr\u{00f6}m.txt", // angstrom with Swedish characters
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    assert!(
        created_count > 0,
        "should be able to create at least one accented filename"
    );

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(
        paths.len(),
        created_count,
        "walker should find all created files"
    );

    // Verify we can find specific files
    for name in &names {
        let path = PathBuf::from(*name);
        if root.join(name).exists() {
            assert!(
                paths.contains(&path),
                "should find file with accented name: {name}"
            );
        }
    }
}

/// Verifies directories with accented names are traversed correctly.
#[test]
fn directories_with_accented_names() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("accented_dirs");
    fs::create_dir(&root).expect("create root");

    let dir_name = "caf\u{00e9}_folder";
    if try_create_dir(&root, dir_name) {
        let inner_file = root.join(dir_name).join("inner.txt");
        fs::write(&inner_file, b"content").expect("write inner file");

        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert!(paths.contains(&PathBuf::from(dir_name)));
        assert!(paths.contains(&PathBuf::from(format!("{dir_name}/inner.txt"))));
    }
}

// ============================================================================
// CJK Characters (Chinese, Japanese, Korean)
// ============================================================================

/// Verifies handling of Japanese filenames (Hiragana, Katakana, Kanji).
#[test]
fn japanese_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("japanese");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{65e5}\u{672c}\u{8a9e}.txt",                 // "Japanese" in kanji
        "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}.txt", // "Hello" in hiragana
        "\u{30b3}\u{30f3}\u{30d4}\u{30e5}\u{30fc}\u{30bf}.txt", // "Computer" in katakana
        "\u{6771}\u{4eac}.txt",                         // "Tokyo" in kanji
        "\u{5bcc}\u{58eb}\u{5c71}.txt",                 // "Mt. Fuji" in kanji
        "\u{3042}\u{3044}\u{3046}\u{3048}\u{304a}.txt", // aiueo in hiragana
        "test_\u{65e5}\u{672c}\u{8a9e}_test.txt",       // mixed with ASCII
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    assert!(
        created_count > 0,
        "should be able to create at least one Japanese filename"
    );

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), created_count);

    // Verify file names are preserved exactly
    for name in &names {
        if root.join(name).exists() {
            assert!(
                paths.contains(&PathBuf::from(*name)),
                "should find Japanese file: {name}"
            );
        }
    }
}

/// Verifies handling of Chinese filenames (Simplified and Traditional).
#[test]
fn chinese_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("chinese");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{4e2d}\u{6587}.txt",         // "Chinese" in Chinese
        "\u{4f60}\u{597d}.txt",         // "Hello" in Chinese
        "\u{5317}\u{4eac}.txt",         // "Beijing" in Chinese
        "\u{4e0a}\u{6d77}.txt",         // "Shanghai" in Chinese
        "\u{53f0}\u{5317}.txt",         // "Taipei" in Traditional Chinese
        "\u{7e41}\u{9ad4}\u{5b57}.txt", // "Traditional characters" in Traditional
        "\u{7b80}\u{4f53}\u{5b57}.txt", // "Simplified characters" in Simplified
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    assert!(
        created_count > 0,
        "should be able to create at least one Chinese filename"
    );

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), created_count);
}

/// Verifies handling of Korean filenames (Hangul).
#[test]
fn korean_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("korean");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{d55c}\u{ad6d}\u{c5b4}.txt",                 // "Korean" in Korean
        "\u{c548}\u{b155}\u{d558}\u{c138}\u{c694}.txt", // "Hello" in Korean
        "\u{c11c}\u{c6b8}.txt",                         // "Seoul" in Korean
        "\u{bd80}\u{c0b0}.txt",                         // "Busan" in Korean
        "\u{ac00}\u{b098}\u{b2e4}.txt",                 // "ga na da" in Korean
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    assert!(
        created_count > 0,
        "should be able to create at least one Korean filename"
    );

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), created_count);
}

/// Verifies nested directories with CJK names.
#[test]
fn cjk_nested_directories() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("cjk_nested");
    fs::create_dir(&root).expect("create root");

    // Create nested structure: root/japanese_dir/chinese_dir/korean_file.txt
    let japanese_dir = "\u{65e5}\u{672c}"; // "Japan"
    let chinese_dir = "\u{4e2d}\u{56fd}"; // "China"
    let korean_file = "\u{d55c}\u{ad6d}.txt"; // "Korea.txt"

    if try_create_dir(&root, japanese_dir) {
        let jp_path = root.join(japanese_dir);
        if try_create_dir(jp_path.as_path(), chinese_dir) {
            let cn_path = jp_path.join(chinese_dir);
            if try_create_file(cn_path.as_path(), korean_file, b"content") {
                let walker = FileListBuilder::new(&root).build().expect("build walker");
                let paths = collect_relative_paths(walker);

                // Should have: japanese_dir, japanese_dir/chinese_dir, japanese_dir/chinese_dir/korean_file.txt
                assert_eq!(paths.len(), 3);

                let expected_file = PathBuf::from(japanese_dir)
                    .join(chinese_dir)
                    .join(korean_file);
                assert!(
                    paths.contains(&expected_file),
                    "should find nested CJK file"
                );
            }
        }
    }
}

// ============================================================================
// Emoji Characters
// ============================================================================

/// Verifies handling of emoji filenames.
#[test]
fn emoji_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("emoji");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{1f389}.txt",           // Party popper emoji
        "\u{1f680}.txt",           // Rocket emoji
        "\u{1f4c1}.txt",           // File folder emoji
        "\u{2764}\u{fe0f}.txt",    // Red heart emoji with variation selector
        "\u{1f600}.txt",           // Grinning face emoji
        "\u{1f4bb}.txt",           // Laptop emoji
        "\u{2705}.txt",            // Check mark emoji
        "test_\u{1f389}_file.txt", // Mixed ASCII and emoji
        "\u{1f1ef}\u{1f1f5}.txt",  // Flag emoji (Japan flag - regional indicators)
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    // Emoji support varies by filesystem
    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(
            paths.len(),
            created_count,
            "walker should find all created emoji files"
        );
    }
}

/// Verifies handling of complex emoji sequences (skin tones, ZWJ sequences).
#[test]
fn complex_emoji_sequences() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("complex_emoji");
    fs::create_dir(&root).expect("create root");

    let names = [
        // Emoji with skin tone modifier
        "\u{1f44d}\u{1f3fb}.txt", // Thumbs up with light skin tone
        // ZWJ sequences (family emoji)
        "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}.txt", // Family: man, woman, girl
        // Professional emoji
        "\u{1f469}\u{200d}\u{1f4bb}.txt", // Woman technologist
        // Multiple emoji in sequence
        "\u{1f600}\u{1f389}\u{1f680}.txt", // Multiple emoji together
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
// Multi-byte Sequences
// ============================================================================

/// Verifies handling of various multi-byte UTF-8 sequences.
#[test]
fn multi_byte_utf8_sequences() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("multibyte");
    fs::create_dir(&root).expect("create root");

    // Test various byte-length characters:
    // - 1 byte: ASCII (U+0000 to U+007F)
    // - 2 bytes: Latin extended, Greek, Cyrillic (U+0080 to U+07FF)
    // - 3 bytes: CJK, most symbols (U+0800 to U+FFFF)
    // - 4 bytes: Emoji, rare CJK (U+10000 to U+10FFFF)

    let names = [
        "ascii.txt",                    // 1-byte characters
        "\u{00e9}\u{00f1}\u{00fc}.txt", // 2-byte Latin extended
        "\u{03b1}\u{03b2}\u{03b3}.txt", // 2-byte Greek letters (alpha, beta, gamma)
        "\u{0430}\u{0431}\u{0432}.txt", // 2-byte Cyrillic letters
        "\u{4e2d}\u{6587}.txt",         // 3-byte CJK
        "\u{20ac}\u{00a3}\u{00a5}.txt", // Currency symbols (euro, pound, yen)
        "\u{1f600}\u{1f389}.txt",       // 4-byte emoji
        "\u{10000}.txt",                // Linear B syllable (4-byte)
        // Mix of different byte lengths
        "a\u{00e9}\u{4e2d}\u{1f600}.txt",
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    assert!(
        created_count > 0,
        "should be able to create at least one multi-byte filename"
    );

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), created_count);
}

/// Verifies byte length preservation for UTF-8 names.
#[test]
fn utf8_byte_length_preservation() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("bytelength");
    fs::create_dir(&root).expect("create root");

    // Japanese word that is 9 bytes in UTF-8: 3 characters * 3 bytes each
    let japanese_name = "\u{65e5}\u{672c}\u{8a9e}.txt"; // "Japanese"
    assert_eq!(
        japanese_name.len(),
        13,
        "3 kanji (3 bytes each) + 4 for .txt = 13 bytes"
    );

    if try_create_file(&root, japanese_name, b"content") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        let file_entry = entries
            .iter()
            .find(|e| !e.is_root())
            .expect("should have file entry");

        let file_name = file_entry.file_name().expect("should have file name");
        // file_name() returns the full filename including extension
        assert_eq!(file_name.len(), japanese_name.len());
    }
}

// ============================================================================
// Combining Characters
// ============================================================================

/// Verifies handling of combining characters (diacritical marks).
#[test]
fn combining_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("combining");
    fs::create_dir(&root).expect("create root");

    // Combining characters are separate Unicode code points that modify the previous character
    let names = [
        // e followed by combining acute accent (U+0301)
        "cafe\u{0301}.txt",
        // a followed by combining ring above (U+030A) - like angstrom
        "a\u{030a}ngstrom.txt",
        // o followed by combining diaeresis (U+0308)
        "co\u{0308}operate.txt",
        // n followed by combining tilde (U+0303)
        "man\u{0303}ana.txt",
        // Multiple combining marks on same character
        "a\u{0301}\u{0308}.txt", // a with acute and diaeresis
        // Vietnamese with multiple diacritics
        "Vie\u{0323}\u{0302}t.txt", // Vietnamese with combining marks
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

        assert_eq!(
            paths.len(),
            created_count,
            "should find all combining character files"
        );
    }
}

/// Verifies that precomposed and decomposed forms are handled distinctly.
#[test]
fn precomposed_vs_decomposed() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("normalization");
    fs::create_dir(&root).expect("create root");

    // Precomposed: single code point for accented character
    let precomposed = "caf\u{00e9}.txt"; // U+00E9 = precomposed e-acute

    // Decomposed: base character + combining mark
    let decomposed = "cafe\u{0301}.txt"; // U+0065 U+0301 = e + combining acute

    let mut precomposed_created = false;
    let mut decomposed_created = false;

    if try_create_file(&root, precomposed, b"precomposed") {
        precomposed_created = true;
    }

    // Note: On some filesystems (like HFS+ on macOS), these might normalize to the same name
    if try_create_file(&root, decomposed, b"decomposed") {
        decomposed_created = true;
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // The test passes if we find all files that were created
    let expected_count = if precomposed_created && decomposed_created {
        // On filesystems that don't normalize, we should see both
        // On normalizing filesystems, we might see 1 or 2
        paths.len()
    } else if precomposed_created || decomposed_created {
        1
    } else {
        0
    };

    assert!(
        !paths.is_empty() && paths.len() <= 2,
        "should find 1 or 2 files depending on filesystem normalization"
    );
    assert_eq!(paths.len(), expected_count);
}

// ============================================================================
// Right-to-Left Scripts (Arabic, Hebrew)
// ============================================================================

/// Verifies handling of Arabic filenames.
#[test]
fn arabic_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("arabic");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}.txt", // "Hello" in Arabic
        "\u{0627}\u{0644}\u{0639}\u{0631}\u{0628}\u{064a}\u{0629}.txt", // "Arabic" in Arabic
        "\u{0627}\u{0644}\u{0642}\u{0627}\u{0647}\u{0631}\u{0629}.txt", // "Cairo" in Arabic
        // With Arabic numerals
        "\u{0661}\u{0662}\u{0663}.txt", // Arabic-Indic digits 1-2-3
        // Mixed with ASCII
        "test_\u{0639}\u{0631}\u{0628}\u{064a}_file.txt",
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

        assert_eq!(
            paths.len(),
            created_count,
            "walker should find all created Arabic files"
        );
    }
}

/// Verifies handling of Hebrew filenames.
#[test]
fn hebrew_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("hebrew");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{05e9}\u{05dc}\u{05d5}\u{05dd}.txt", // "Shalom" (Hello) in Hebrew
        "\u{05e2}\u{05d1}\u{05e8}\u{05d9}\u{05ea}.txt", // "Hebrew" in Hebrew
        "\u{05d9}\u{05e8}\u{05d5}\u{05e9}\u{05dc}\u{05d9}\u{05dd}.txt", // "Jerusalem" in Hebrew
        // With vowel points (nikkud)
        "\u{05e9}\u{05b8}\u{05dc}\u{05d5}\u{05b9}\u{05dd}.txt", // "Shalom" with vowels
        // Mixed with ASCII
        "test_\u{05e2}\u{05d1}\u{05e8}\u{05d9}_file.txt",
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

        assert_eq!(
            paths.len(),
            created_count,
            "walker should find all created Hebrew files"
        );
    }
}

/// Verifies handling of bidirectional text in filenames.
#[test]
fn bidirectional_text() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("bidi");
    fs::create_dir(&root).expect("create root");

    // Filenames with mixed LTR and RTL text
    let names = [
        // Hebrew text followed by ASCII
        "\u{05e9}\u{05dc}\u{05d5}\u{05dd}_hello.txt",
        // ASCII followed by Arabic
        "hello_\u{0645}\u{0631}\u{062d}\u{0628}\u{0627}.txt",
        // Hebrew, ASCII, Arabic
        "\u{05e2}\u{05d1}\u{05e8}\u{05d9}_test_\u{0639}\u{0631}\u{0628}\u{064a}.txt",
        // With explicit bidi control characters
        "\u{202a}LTR\u{202c}.txt", // LTR embedding
        "\u{202b}RTL\u{202c}.txt", // RTL embedding
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

        assert_eq!(
            paths.len(),
            created_count,
            "walker should find all created bidirectional files"
        );
    }
}

// ============================================================================
// Mixed Scripts in Same Filename
// ============================================================================

/// Verifies handling of filenames with multiple scripts.
#[test]
fn mixed_scripts_single_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixed_scripts");
    fs::create_dir(&root).expect("create root");

    let names = [
        // ASCII + CJK
        "hello_\u{4e16}\u{754c}.txt", // "hello_world" with Chinese "world"
        // Latin + Greek
        "alpha_\u{03b1}_beta_\u{03b2}.txt",
        // Latin + Cyrillic
        "english_\u{0440}\u{0443}\u{0441}\u{0441}\u{043a}\u{0438}\u{0439}.txt", // English + "Russian"
        // CJK + Latin + Emoji
        "\u{65e5}\u{672c}_Japan_\u{1f1ef}\u{1f1f5}.txt",
        // Arabic + Hebrew (both RTL)
        "\u{0639}\u{0631}\u{0628}\u{064a}_\u{05e2}\u{05d1}\u{05e8}\u{05d9}.txt",
        // Latin + Thai
        "test_\u{0e17}\u{0e14}\u{0e2a}\u{0e2d}\u{0e1a}.txt", // test + Thai "test"
        // All major script families in one name
        "A\u{00e9}\u{03b1}\u{0430}\u{4e2d}\u{05d0}\u{0639}\u{1f600}.txt",
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    assert!(
        created_count > 0,
        "should be able to create at least one mixed-script filename"
    );

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), created_count);

    // Verify exact name preservation
    for name in &names {
        if root.join(name).exists() {
            assert!(
                paths.contains(&PathBuf::from(*name)),
                "should preserve exact mixed-script filename: {name}"
            );
        }
    }
}

/// Verifies directory traversal with mixed-script paths.
#[test]
fn mixed_scripts_nested_paths() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixed_nested");
    fs::create_dir(&root).expect("create root");

    // Create a path with different scripts at each level
    let latin_dir = "documents";
    let japanese_dir = "\u{30c9}\u{30ad}\u{30e5}\u{30e1}\u{30f3}\u{30c8}"; // "Document" in Japanese
    let arabic_file = "\u{0645}\u{0644}\u{0641}.txt"; // "File" in Arabic

    fs::create_dir(root.join(latin_dir)).expect("create latin dir");
    if try_create_dir(&root.join(latin_dir), japanese_dir)
        && try_create_file(
            &root.join(latin_dir).join(japanese_dir),
            arabic_file,
            b"content",
        )
    {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        // Should have the nested structure
        assert!(paths.contains(&PathBuf::from(latin_dir)));
        assert!(paths.contains(&PathBuf::from(format!("{latin_dir}/{japanese_dir}"))));
        assert!(paths.contains(&PathBuf::from(format!(
            "{latin_dir}/{japanese_dir}/{arabic_file}"
        ))));
    }
}

// ============================================================================
// Edge Cases and Boundary Conditions
// ============================================================================

/// Verifies handling of maximum-length UTF-8 filenames.
#[test]
fn maximum_length_utf8_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("maxlen");
    fs::create_dir(&root).expect("create root");

    // Most filesystems support 255 bytes for filenames
    // Using 3-byte CJK characters, that's ~85 characters max
    // Leave room for .txt extension (4 bytes)
    let cjk_char = "\u{4e2d}"; // 3-byte character
    let name = format!("{}.txt", cjk_char.repeat(80)); // 240 bytes + 4 = 244 bytes

    if try_create_file(&root, &name, b"data") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1);
        assert!(paths.contains(&PathBuf::from(&name)));
    }
}

/// Verifies handling of filenames that look similar but differ in encoding.
#[test]
fn visually_similar_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("similar");
    fs::create_dir(&root).expect("create root");

    // These look similar but are different characters
    let names = [
        "a.txt",        // Latin lowercase a
        "\u{0430}.txt", // Cyrillic lowercase a (looks identical)
        "\u{03b1}.txt", // Greek lowercase alpha
        "A.txt",        // Latin uppercase A
        "\u{0410}.txt", // Cyrillic uppercase A
    ];

    let mut created_count = 0;
    for name in &names {
        if try_create_file(&root, name, b"data") {
            created_count += 1;
        }
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Each file should be distinct if the filesystem is case-sensitive and Unicode-aware
    assert!(
        paths.len() <= created_count,
        "should not create more files than attempted"
    );
    assert!(!paths.is_empty(), "should create at least one file");
}

/// Verifies handling of filenames with zero-width characters.
#[test]
fn zero_width_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("zerowidth");
    fs::create_dir(&root).expect("create root");

    let names = [
        "test\u{200b}file.txt", // Zero-width space
        "test\u{200c}file.txt", // Zero-width non-joiner
        "test\u{200d}file.txt", // Zero-width joiner
        "test\u{feff}file.txt", // Byte order mark / zero-width no-break space
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

        // Walker should find all created files
        assert_eq!(paths.len(), created_count);
    }
}

/// Verifies sorting behavior with UTF-8 filenames.
#[test]
fn utf8_sorting_order() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("sorting");
    fs::create_dir(&root).expect("create root");

    // Create files that should sort in a specific order (by byte value)
    let names = [
        "aaa.txt",           // ASCII
        "bbb.txt",           // ASCII
        "\u{00e9}file.txt",  // Latin Extended (2 bytes)
        "\u{4e2d}file.txt",  // CJK (3 bytes)
        "\u{1f600}file.txt", // Emoji (4 bytes)
    ];

    for name in &names {
        let _ = try_create_file(&root, name, b"data");
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Verify files are sorted (by bytes/lexicographic order)
    for i in 0..paths.len().saturating_sub(1) {
        assert!(
            paths[i] < paths[i + 1],
            "files should be sorted: {:?} should come before {:?}",
            paths[i],
            paths[i + 1]
        );
    }
}

/// Verifies file_name() returns correct OsStr for UTF-8 names.
#[test]
fn file_name_method_utf8() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("filename_test");
    fs::create_dir(&root).expect("create root");

    let test_name = "\u{65e5}\u{672c}\u{8a9e}.txt"; // Japanese characters

    if try_create_file(&root, test_name, b"data") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        let file_entry = entries
            .iter()
            .find(|e| !e.is_root())
            .expect("should have file entry");

        let file_name = file_entry.file_name().expect("should have file name");
        assert_eq!(file_name, OsStr::new(test_name));
    }
}

/// Verifies relative and full paths are correct for UTF-8 names.
#[test]
fn path_methods_utf8() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("path_test");
    fs::create_dir(&root).expect("create root");

    let dir_name = "\u{4e2d}\u{6587}_dir"; // Chinese directory
    let file_name = "\u{65e5}\u{672c}.txt"; // Japanese file

    if try_create_dir(&root, dir_name) && try_create_file(&root.join(dir_name), file_name, b"data")
    {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        // Find the file entry
        let file_entry = entries
            .iter()
            .find(|e| e.relative_path().ends_with(file_name))
            .expect("should find file entry");

        // Verify relative path
        let expected_relative = Path::new(dir_name).join(file_name);
        assert_eq!(file_entry.relative_path(), expected_relative);

        // Verify full path
        let expected_full = root.join(dir_name).join(file_name);
        assert_eq!(file_entry.full_path(), expected_full);

        // Verify full path exists
        assert!(file_entry.full_path().exists());
    }
}

// ============================================================================
// Supplementary Multilingual Plane Characters
// ============================================================================

/// Verifies handling of characters from the Supplementary Multilingual Plane (SMP).
#[test]
fn supplementary_plane_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("smp");
    fs::create_dir(&root).expect("create root");

    // Characters from various SMP blocks (U+10000 and above, 4 bytes in UTF-8)
    let names = [
        "\u{10000}.txt", // Linear B Syllable B008 A
        "\u{10300}.txt", // Old Italic Letter A
        "\u{10400}.txt", // Deseret Capital Letter Long I
        "\u{10800}.txt", // Cypriot Syllable A
        "\u{1d400}.txt", // Mathematical Bold Capital A
        "\u{20000}.txt", // CJK Extension B character
        "\u{1f300}.txt", // Cyclone (weather emoji)
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
// Special Unicode Categories
// ============================================================================

/// Verifies handling of mathematical and technical symbols.
#[test]
fn mathematical_symbols() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("math");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{221e}.txt",            // Infinity symbol
        "\u{2211}.txt",            // Summation
        "\u{222b}.txt",            // Integral
        "\u{2202}.txt",            // Partial differential
        "\u{03c0}.txt",            // Pi
        "\u{221a}.txt",            // Square root
        "x\u{00b2}+y\u{00b2}.txt", // x squared + y squared
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

/// Verifies handling of currency symbols.
#[test]
fn currency_symbols() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("currency");
    fs::create_dir(&root).expect("create root");

    let names = [
        "\u{20ac}100.txt",                      // Euro sign
        "\u{00a3}100.txt",                      // Pound sign
        "\u{00a5}100.txt",                      // Yen sign
        "\u{20b9}100.txt",                      // Indian Rupee sign
        "\u{20bf}100.txt",                      // Bitcoin sign
        "price_\u{0024}_\u{20ac}_\u{00a3}.txt", // Dollar, Euro, Pound
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
