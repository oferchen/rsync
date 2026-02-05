//! Integration tests for maximum filename length handling.
//!
//! These tests verify correct handling of filenames at or near filesystem
//! limits, typically 255 bytes per filename component. Tests cover:
//! - Files with 255-character names
//! - Directories with 255-character names
//! - Path combining and traversal with long names
//! - Transfer operations with long filenames
//! - Edge cases around the 255-byte boundary
//!
//! Reference: rsync 3.4.1 flist.c, POSIX NAME_MAX (255 bytes)

use flist::{FileListBuilder, FileListEntry, FileListError};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

// ============================================================================
// Constants
// ============================================================================

/// Maximum filename length in bytes for most POSIX filesystems.
/// This is NAME_MAX, not PATH_MAX (which is typically 4096).
const MAX_FILENAME_BYTES: usize = 255;

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

/// Creates a filename that is exactly the specified number of bytes.
/// Uses ASCII characters for simplicity and compatibility.
fn create_max_length_filename(length: usize, suffix: &str) -> String {
    assert!(length >= suffix.len(), "length must be >= suffix length");
    let padding_len = length - suffix.len();
    format!("{}{}", "a".repeat(padding_len), suffix)
}

/// Attempts to create a file with the given name, returning true if successful.
fn try_create_file(dir: &Path, name: &str, content: &[u8]) -> bool {
    fs::write(dir.join(name), content).is_ok()
}

/// Attempts to create a directory with the given name, returning true if successful.
fn try_create_dir(dir: &Path, name: &str) -> bool {
    fs::create_dir(dir.join(name)).is_ok()
}

// ============================================================================
// 1. Maximum Filename Length Tests
// ============================================================================

/// Verifies handling of a file with exactly 255-byte filename.
#[test]
fn file_with_255_byte_name() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("max_name");
    fs::create_dir(&root).expect("create root");

    // Create filename that is exactly 255 bytes
    let filename = create_max_length_filename(MAX_FILENAME_BYTES, ".txt");
    assert_eq!(
        filename.len(),
        MAX_FILENAME_BYTES,
        "filename should be exactly 255 bytes"
    );

    if try_create_file(&root, &filename, b"test content") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1, "should find one file");
        assert_eq!(
            paths[0],
            PathBuf::from(&filename),
            "should preserve exact filename"
        );

        // Verify file content is readable
        let content = fs::read(root.join(&filename)).expect("read file");
        assert_eq!(content, b"test content");
    } else {
        eprintln!("Filesystem does not support 255-byte filenames, test skipped");
    }
}

/// Verifies handling of multiple files with maximum-length names.
#[test]
fn multiple_files_with_max_length_names() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("multi_max");
    fs::create_dir(&root).expect("create root");

    // Create several files with 255-byte names, differentiated by suffix
    let filenames = vec![
        create_max_length_filename(MAX_FILENAME_BYTES, "1.txt"),
        create_max_length_filename(MAX_FILENAME_BYTES, "2.txt"),
        create_max_length_filename(MAX_FILENAME_BYTES, "3.txt"),
    ];

    let mut created_count = 0;
    for filename in &filenames {
        if try_create_file(&root, filename, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), created_count, "should find all created files");

        // Verify all created files are found
        for filename in &filenames {
            if root.join(filename).exists() {
                assert!(
                    paths.contains(&PathBuf::from(filename)),
                    "should find file with max-length name: {filename}"
                );
            }
        }

        // Verify files are sorted correctly
        for i in 0..paths.len().saturating_sub(1) {
            assert!(
                paths[i] < paths[i + 1],
                "files should be sorted lexicographically"
            );
        }
    }
}

/// Verifies handling of filenames just under the 255-byte limit.
#[test]
fn file_with_254_byte_name() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("near_max");
    fs::create_dir(&root).expect("create root");

    let filename = create_max_length_filename(254, ".txt");
    assert_eq!(filename.len(), 254);

    fs::write(root.join(&filename), b"content").expect("create file");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from(&filename));
}

/// Verifies handling of filenames at various lengths approaching maximum.
#[test]
fn files_with_varying_long_names() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("varying_lengths");
    fs::create_dir(&root).expect("create root");

    // Test lengths: 240, 245, 250, 253, 254, 255
    let test_lengths = vec![240, 245, 250, 253, 254, 255];
    let mut created_count = 0;

    for length in &test_lengths {
        let filename = create_max_length_filename(*length, ".txt");
        if try_create_file(&root, &filename, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), created_count);
    }
}

/// Verifies metadata access for files with maximum-length names.
#[test]
fn metadata_access_max_length_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metadata_test");
    fs::create_dir(&root).expect("create root");

    let filename = create_max_length_filename(MAX_FILENAME_BYTES, ".txt");
    let test_content = b"test content for metadata verification";

    if try_create_file(&root, &filename, test_content) {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        let file_entry = entries
            .iter()
            .find(|e| !e.is_root())
            .expect("should find file entry");

        // Verify metadata is accessible
        assert!(file_entry.metadata().is_file());
        assert_eq!(file_entry.metadata().len(), test_content.len() as u64);

        // Verify file_name() returns correct value
        assert_eq!(
            file_entry.file_name(),
            Some(OsStr::new(&filename)),
            "file_name() should match"
        );

        // Verify relative_path() is correct
        assert_eq!(file_entry.relative_path(), Path::new(&filename));

        // Verify full_path() is correct and exists
        assert_eq!(file_entry.full_path(), root.join(&filename));
        assert!(file_entry.full_path().exists());
    }
}

// ============================================================================
// 2. Maximum Directory Name Length Tests
// ============================================================================

/// Verifies handling of a directory with exactly 255-byte name.
#[test]
fn directory_with_255_byte_name() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("max_dir");
    fs::create_dir(&root).expect("create root");

    let dirname = create_max_length_filename(MAX_FILENAME_BYTES, "_dir");
    assert_eq!(dirname.len(), MAX_FILENAME_BYTES);

    if try_create_dir(&root, &dirname) {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(&dirname));

        // Verify it's recognized as a directory
        let entries =
            collect_all_entries(FileListBuilder::new(&root).build().expect("build walker"));
        let dir_entry = entries.iter().find(|e| !e.is_root()).expect("find dir");
        assert!(dir_entry.metadata().is_dir());
    }
}

/// Verifies traversal into directory with maximum-length name.
#[test]
fn traverse_into_max_length_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("traverse_max");
    fs::create_dir(&root).expect("create root");

    let dirname = create_max_length_filename(MAX_FILENAME_BYTES, "_dir");

    if try_create_dir(&root, &dirname) {
        let dir_path = root.join(&dirname);
        fs::write(dir_path.join("inner.txt"), b"inner content").expect("create inner file");

        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        // Should have directory and file
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&PathBuf::from(&dirname)));
        assert!(paths.contains(&PathBuf::from(format!("{dirname}/inner.txt"))));
    }
}

/// Verifies multiple files in directory with maximum-length name.
#[test]
fn multiple_files_in_max_length_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("multi_in_max");
    fs::create_dir(&root).expect("create root");

    let dirname = create_max_length_filename(MAX_FILENAME_BYTES, "_dir");

    if try_create_dir(&root, &dirname) {
        let dir_path = root.join(&dirname);

        // Create multiple files inside
        for i in 0..5 {
            fs::write(dir_path.join(format!("file{i}.txt")), b"data")
                .expect("create file in max dir");
        }

        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        // Should have 1 directory + 5 files = 6 entries
        assert_eq!(paths.len(), 6);

        // Verify directory is found
        assert!(paths.contains(&PathBuf::from(&dirname)));

        // Verify all files are found
        for i in 0..5 {
            let expected = PathBuf::from(format!("{dirname}/file{i}.txt"));
            assert!(
                paths.contains(&expected),
                "should find file in max-length dir: {expected:?}"
            );
        }
    }
}

/// Verifies nested directories where parent has maximum-length name.
#[test]
fn nested_directories_with_max_length_parent() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("nested_max_parent");
    fs::create_dir(&root).expect("create root");

    let parent_dirname = create_max_length_filename(MAX_FILENAME_BYTES, "_parent");

    if try_create_dir(&root, &parent_dirname) {
        let parent_path = root.join(&parent_dirname);

        // Create child directory with normal name
        fs::create_dir(parent_path.join("child")).expect("create child dir");
        fs::write(parent_path.join("child/file.txt"), b"nested content")
            .expect("create nested file");

        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        // Should have: parent_dir, child_dir, file = 3 entries
        assert_eq!(paths.len(), 3);
        assert!(paths.contains(&PathBuf::from(&parent_dirname)));
        assert!(paths.contains(&PathBuf::from(format!("{parent_dirname}/child"))));
        assert!(paths.contains(&PathBuf::from(format!("{parent_dirname}/child/file.txt"))));
    }
}

// ============================================================================
// 3. Path Combining and Limits Tests
// ============================================================================

/// Verifies file with max-length name inside directory with max-length name.
#[test]
fn max_length_file_in_max_length_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("both_max");
    fs::create_dir(&root).expect("create root");

    let dirname = create_max_length_filename(MAX_FILENAME_BYTES, "_dir");
    let filename = create_max_length_filename(MAX_FILENAME_BYTES, ".txt");

    if try_create_dir(&root, &dirname) {
        let dir_path = root.join(&dirname);
        if try_create_file(&dir_path, &filename, b"content") {
            let walker = FileListBuilder::new(&root).build().expect("build walker");
            let paths = collect_relative_paths(walker);

            // Should have directory and file
            assert_eq!(paths.len(), 2);

            let expected_file_path = PathBuf::from(&dirname).join(&filename);
            assert!(
                paths.contains(&expected_file_path),
                "should find file with path: {expected_file_path:?}"
            );
        }
    }
}

/// Verifies deeply nested structure with multiple max-length directory names.
#[test]
fn deeply_nested_max_length_directories() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("deep_max");
    fs::create_dir(&root).expect("create root");

    // Create 3 levels of max-length directories
    let dir1 = create_max_length_filename(MAX_FILENAME_BYTES, "_d1");
    let dir2 = create_max_length_filename(MAX_FILENAME_BYTES, "_d2");
    let dir3 = create_max_length_filename(MAX_FILENAME_BYTES, "_d3");

    if try_create_dir(&root, &dir1) {
        let path1 = root.join(&dir1);
        if try_create_dir(&path1, &dir2) {
            let path2 = path1.join(&dir2);
            if try_create_dir(&path2, &dir3) {
                let path3 = path2.join(&dir3);
                fs::write(path3.join("deep.txt"), b"deep content").expect("create deep file");

                let walker = FileListBuilder::new(&root).build().expect("build walker");
                let paths = collect_relative_paths(walker);

                // Should have 3 directories + 1 file = 4 entries
                assert_eq!(paths.len(), 4);

                // Verify all levels are found
                assert!(paths.contains(&PathBuf::from(&dir1)));
                assert!(paths.contains(&PathBuf::from(format!("{dir1}/{dir2}"))));
                assert!(paths.contains(&PathBuf::from(format!("{dir1}/{dir2}/{dir3}"))));
                assert!(paths.contains(&PathBuf::from(format!("{dir1}/{dir2}/{dir3}/deep.txt"))));
            }
        }
    }
}

/// Verifies path combining respects component limits but allows long paths.
#[test]
fn long_path_with_max_length_components() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("long_path");
    fs::create_dir(&root).expect("create root");

    // Create path with 5 levels of reasonably long directory names
    // Each component is 200 bytes, well under 255 limit
    let mut current_path = root.clone();
    let dir_names: Vec<String> = (0..5)
        .map(|i| create_max_length_filename(200, &format!("_level{i}")))
        .collect();

    let mut all_created = true;
    for dir_name in &dir_names {
        if !try_create_dir(&current_path, dir_name) {
            all_created = false;
            break;
        }
        current_path = current_path.join(dir_name);
    }

    if all_created {
        fs::write(current_path.join("deep_file.txt"), b"deep content").expect("create deep file");

        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        // Should have 5 directories + 1 file = 6 entries
        assert_eq!(paths.len(), 6);

        // Build expected deepest path
        let mut expected_path = PathBuf::new();
        for dir_name in &dir_names {
            expected_path = expected_path.join(dir_name);
        }
        expected_path = expected_path.join("deep_file.txt");

        assert!(
            paths.contains(&expected_path),
            "should find deeply nested file"
        );
    }
}

/// Verifies relative path computation with max-length names.
#[test]
fn relative_path_with_max_length_names() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("relative_test");
    fs::create_dir(&root).expect("create root");

    let dirname = create_max_length_filename(MAX_FILENAME_BYTES, "_dir");
    let filename = create_max_length_filename(MAX_FILENAME_BYTES, ".txt");

    if try_create_dir(&root, &dirname) {
        let dir_path = root.join(&dirname);
        if try_create_file(&dir_path, &filename, b"content") {
            let walker = FileListBuilder::new(&root).build().expect("build walker");
            let entries = collect_all_entries(walker);

            // Find the file entry
            let file_entry = entries
                .iter()
                .find(|e| !e.is_root() && e.metadata().is_file())
                .expect("find file entry");

            // Verify relative path is correct
            let expected_relative = Path::new(&dirname).join(&filename);
            assert_eq!(
                file_entry.relative_path(),
                expected_relative,
                "relative path should combine max-length components"
            );

            // Verify full path is correct
            let expected_full = root.join(&dirname).join(&filename);
            assert_eq!(file_entry.full_path(), expected_full);
            assert!(file_entry.full_path().exists());
        }
    }
}

// ============================================================================
// 4. Edge Cases and Boundary Conditions
// ============================================================================

/// Verifies sorting with max-length filenames.
#[test]
fn sorting_max_length_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("sorting_max");
    fs::create_dir(&root).expect("create root");

    // Create files that differ only in the last character
    let base = "a".repeat(MAX_FILENAME_BYTES - 5);
    let files = vec![
        format!("{}a.txt", base),
        format!("{}b.txt", base),
        format!("{}c.txt", base),
    ];

    let mut created_count = 0;
    for filename in &files {
        if try_create_file(&root, filename, b"data") {
            created_count += 1;
        }
    }

    if created_count > 0 {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), created_count);

        // Verify sorting is correct
        for i in 0..paths.len().saturating_sub(1) {
            assert!(
                paths[i] < paths[i + 1],
                "max-length files should be sorted correctly"
            );
        }
    }
}

/// Verifies depth tracking with max-length names.
#[test]
fn depth_tracking_with_max_length_names() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("depth_test");
    fs::create_dir(&root).expect("create root");

    let dirname = create_max_length_filename(MAX_FILENAME_BYTES, "_dir");

    if try_create_dir(&root, &dirname) {
        let dir_path = root.join(&dirname);
        fs::write(dir_path.join("file.txt"), b"content").expect("create file");

        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        // Find directory entry
        let dir_entry = entries
            .iter()
            .find(|e| !e.is_root() && e.metadata().is_dir())
            .expect("find dir entry");

        // Find file entry
        let file_entry = entries
            .iter()
            .find(|e| !e.is_root() && e.metadata().is_file())
            .expect("find file entry");

        // Verify depths
        assert_eq!(dir_entry.depth(), 1, "directory should be at depth 1");
        assert_eq!(file_entry.depth(), 2, "file should be at depth 2");
    }
}

/// Verifies empty directory with max-length name.
#[test]
fn empty_directory_with_max_length_name() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("empty_max");
    fs::create_dir(&root).expect("create root");

    let dirname = create_max_length_filename(MAX_FILENAME_BYTES, "_empty");

    if try_create_dir(&root, &dirname) {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        // Should only have the empty directory
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(&dirname));
    }
}

/// Verifies zero-length file with max-length name.
#[test]
fn zero_length_file_with_max_length_name() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("zero_len");
    fs::create_dir(&root).expect("create root");

    let filename = create_max_length_filename(MAX_FILENAME_BYTES, ".txt");

    if try_create_file(&root, &filename, b"") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        let file_entry = entries
            .iter()
            .find(|e| !e.is_root())
            .expect("find file entry");

        assert!(file_entry.metadata().is_file());
        assert_eq!(file_entry.metadata().len(), 0, "file should be empty");
    }
}

/// Verifies file_name() works correctly with max-length names.
#[test]
fn file_name_method_with_max_length() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("file_name_test");
    fs::create_dir(&root).expect("create root");

    let filename = create_max_length_filename(MAX_FILENAME_BYTES, ".txt");

    if try_create_file(&root, &filename, b"data") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let entries = collect_all_entries(walker);

        let file_entry = entries
            .iter()
            .find(|e| !e.is_root())
            .expect("find file entry");

        assert_eq!(
            file_entry.file_name(),
            Some(OsStr::new(&filename)),
            "file_name() should return exact max-length name"
        );
    }
}

/// Verifies hidden file with max-length name (starting with dot).
#[test]
fn hidden_file_with_max_length_name() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("hidden_max");
    fs::create_dir(&root).expect("create root");

    // Start with dot, fill remaining 254 bytes
    let filename = format!(".{}", "a".repeat(MAX_FILENAME_BYTES - 1));
    assert_eq!(filename.len(), MAX_FILENAME_BYTES);

    if try_create_file(&root, &filename, b"hidden content") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(&filename));
    }
}

// ============================================================================
// 5. UTF-8 Filenames at Maximum Byte Length
// ============================================================================

/// Verifies max-length filename using multi-byte UTF-8 characters.
#[test]
fn max_length_utf8_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("utf8_max");
    fs::create_dir(&root).expect("create root");

    // Use 3-byte UTF-8 characters (CJK)
    // 255 bytes / 3 bytes per char = 85 characters max
    // Leave room for extension: (255 - 4) / 3 = 83 characters + ".txt"
    let cjk_char = "\u{4e2d}"; // Chinese character (3 bytes)
    let filename = format!("{}.txt", cjk_char.repeat(83));
    let byte_len = filename.len();

    // Should be at or near 255 bytes: 83 * 3 + 4 = 253 bytes
    assert!(
        byte_len <= MAX_FILENAME_BYTES,
        "UTF-8 filename should be <= 255 bytes, got {byte_len}"
    );

    if try_create_file(&root, &filename, b"utf8 content") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(&filename));

        // Verify file content
        let content = fs::read(root.join(&filename)).expect("read utf8 file");
        assert_eq!(content, b"utf8 content");
    }
}

/// Verifies max-length filename with emoji (4-byte UTF-8).
#[test]
fn max_length_emoji_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("emoji_max");
    fs::create_dir(&root).expect("create root");

    // Use 4-byte UTF-8 emoji characters
    // 255 bytes / 4 bytes per char = 63 characters max
    // Leave room for extension: (255 - 4) / 4 = 62 emojis + ".txt"
    let emoji = "\u{1f600}"; // Grinning face (4 bytes)
    let filename = format!("{}.txt", emoji.repeat(62));
    let byte_len = filename.len();

    // Should be at or near 255 bytes: 62 * 4 + 4 = 252 bytes
    assert!(
        byte_len <= MAX_FILENAME_BYTES,
        "emoji filename should be <= 255 bytes, got {byte_len}"
    );

    if try_create_file(&root, &filename, b"emoji content") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(&filename));
    }
}

/// Verifies max-length filename with mixed ASCII and UTF-8.
#[test]
fn max_length_mixed_utf8_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixed_utf8_max");
    fs::create_dir(&root).expect("create root");

    // Mix ASCII and UTF-8 to reach close to 255 bytes
    let ascii_part = "test_";
    let cjk_char = "\u{4e2d}"; // 3 bytes
    let remaining_bytes = MAX_FILENAME_BYTES - ascii_part.len() - 4; // Leave room for .txt
    let cjk_count = remaining_bytes / 3;
    let filename = format!("{}{}.txt", ascii_part, cjk_char.repeat(cjk_count));

    let byte_len = filename.len();
    assert!(byte_len <= MAX_FILENAME_BYTES);

    if try_create_file(&root, &filename, b"mixed content") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(&filename));
    }
}

// ============================================================================
// 6. Special Characters with Max Length
// ============================================================================

/// Verifies max-length filename with spaces.
#[test]
fn max_length_filename_with_spaces() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces_max");
    fs::create_dir(&root).expect("create root");

    // Create filename with spaces at max length
    let filename = format!("{} file.txt", "space".repeat(49)); // ~250 bytes
    let byte_len = filename.len();

    if byte_len <= MAX_FILENAME_BYTES && try_create_file(&root, &filename, b"spaced content") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(&filename));
    }
}

/// Verifies max-length filename with special shell characters.
#[test]
fn max_length_filename_with_special_chars() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("special_max");
    fs::create_dir(&root).expect("create root");

    // Create filename with special characters approaching max length
    let base = "a".repeat(MAX_FILENAME_BYTES - 20);
    let filename = format!("{base}*?[].txt");
    let byte_len = filename.len();

    if byte_len <= MAX_FILENAME_BYTES && try_create_file(&root, &filename, b"special content") {
        let walker = FileListBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);

        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], PathBuf::from(&filename));
    }
}
