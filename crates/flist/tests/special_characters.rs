//! Integration tests for special characters in filenames.
//!
//! These tests verify correct handling of filenames containing special
//! characters that might cause issues in shell operations, protocol
//! encoding, or path manipulation.
//!
//! Reference: rsync 3.4.1 flist.c, io.c for protocol encoding

use flist::{FileListBuilder, FileListEntry, FileListError};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
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

/// Creates a file with the given name and verifies it was created successfully.
fn create_test_file(root: &Path, name: &OsStr) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, b"test content").expect("write test file");
    path
}

/// Creates a directory with the given name and verifies it was created successfully.
fn create_test_dir(root: &Path, name: &OsStr) -> PathBuf {
    let path = root.join(name);
    fs::create_dir(&path).expect("create test directory");
    path
}

// ============================================================================
// 1. Spaces in Filenames
// ============================================================================

/// Verifies handling of single space in filename.
#[test]
fn single_space_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file with space.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file with space.txt"));
}

/// Verifies handling of multiple consecutive spaces.
#[test]
fn multiple_consecutive_spaces() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file  with   multiple    spaces.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from("file  with   multiple    spaces.txt")
    );
}

/// Verifies handling of leading space in filename.
#[test]
fn leading_space_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new(" leading_space.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from(" leading_space.txt"));
}

/// Verifies handling of trailing space in filename.
#[test]
fn trailing_space_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("trailing_space.txt "));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("trailing_space.txt "));
}

/// Verifies handling of space-only filename.
#[test]
fn space_only_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new(" "));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from(" "));
}

/// Verifies handling of multiple spaces only filename.
#[test]
fn multiple_spaces_only_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("   "));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("   "));
}

/// Verifies handling of directory with spaces.
#[test]
fn directory_with_spaces() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("spaces");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new("dir with spaces"));
    create_test_file(&dir_path, OsStr::new("inner file.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("dir with spaces")));
    assert!(paths.contains(&PathBuf::from("dir with spaces/inner file.txt")));
}

// ============================================================================
// 2. Quotes (Single and Double)
// ============================================================================

/// Verifies handling of single quote in filename.
#[test]
fn single_quote_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("quotes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file'with'quotes.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file'with'quotes.txt"));
}

/// Verifies handling of double quote in filename.
#[test]
fn double_quote_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("quotes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file\"with\"quotes.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file\"with\"quotes.txt"));
}

/// Verifies handling of mixed quotes in filename.
#[test]
fn mixed_quotes_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("quotes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file'and\"mixed.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file'and\"mixed.txt"));
}

/// Verifies handling of consecutive quotes.
#[test]
fn consecutive_quotes() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("quotes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file''\"\"quotes.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file''\"\"quotes.txt"));
}

/// Verifies handling of quotes with spaces.
#[test]
fn quotes_with_spaces() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("quotes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file 'quoted part' here.txt"));
    create_test_file(&root, OsStr::new("file \"quoted part\" here.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("file 'quoted part' here.txt")));
    assert!(paths.contains(&PathBuf::from("file \"quoted part\" here.txt")));
}

/// Verifies handling of directory with quotes.
#[test]
fn directory_with_quotes() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("quotes");
    fs::create_dir(&root).expect("create root");

    let dir1 = create_test_dir(&root, OsStr::new("dir'with'single"));
    let dir2 = create_test_dir(&root, OsStr::new("dir\"with\"double"));

    create_test_file(&dir1, OsStr::new("inner.txt"));
    create_test_file(&dir2, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from("dir'with'single")));
    assert!(paths.contains(&PathBuf::from("dir\"with\"double")));
}

// ============================================================================
// 3. Backslashes
// ============================================================================

/// Verifies handling of backslash in filename.
#[test]
fn backslash_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("backslash");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file\\with\\backslash.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file\\with\\backslash.txt"));
}

/// Verifies handling of consecutive backslashes.
#[test]
fn consecutive_backslashes() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("backslash");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file\\\\double.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file\\\\double.txt"));
}

/// Verifies handling of backslash at end of filename.
#[test]
fn trailing_backslash() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("backslash");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file_trailing\\"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file_trailing\\"));
}

/// Verifies handling of backslash with escape-like sequences.
#[test]
fn backslash_escape_sequences() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("backslash");
    fs::create_dir(&root).expect("create root");

    // These look like escape sequences but are just literal characters
    create_test_file(&root, OsStr::new("file\\n.txt"));
    create_test_file(&root, OsStr::new("file\\t.txt"));
    create_test_file(&root, OsStr::new("file\\r.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 3);
    assert!(paths.contains(&PathBuf::from("file\\n.txt")));
    assert!(paths.contains(&PathBuf::from("file\\t.txt")));
    assert!(paths.contains(&PathBuf::from("file\\r.txt")));
}

/// Verifies handling of directory with backslash.
#[test]
fn directory_with_backslash() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("backslash");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new("dir\\with\\backslash"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("dir\\with\\backslash")));
    assert!(paths.contains(&PathBuf::from("dir\\with\\backslash/inner.txt")));
}

// ============================================================================
// 4. Newlines in Filenames
// ============================================================================

/// Verifies handling of newline in filename.
#[test]
fn newline_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("newline");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file\nwith\nnewline.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file\nwith\nnewline.txt"));
}

/// Verifies handling of carriage return in filename.
#[test]
fn carriage_return_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("newline");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file\rwith\rcarriage.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file\rwith\rcarriage.txt"));
}

/// Verifies handling of CRLF in filename.
#[test]
fn crlf_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("newline");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file\r\nwith\r\ncrlf.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file\r\nwith\r\ncrlf.txt"));
}

/// Verifies handling of directory with newline.
#[test]
fn directory_with_newline() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("newline");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new("dir\nwith\nnewline"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("dir\nwith\nnewline")));
    assert!(paths.contains(&PathBuf::from("dir\nwith\nnewline/inner.txt")));
}

// ============================================================================
// 5. Tab Characters
// ============================================================================

/// Verifies handling of tab in filename.
#[test]
fn tab_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tab");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file\twith\ttab.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file\twith\ttab.txt"));
}

/// Verifies handling of leading tab in filename.
#[test]
fn leading_tab_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tab");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("\tleading_tab.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("\tleading_tab.txt"));
}

/// Verifies handling of trailing tab in filename.
#[test]
fn trailing_tab_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tab");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("trailing_tab.txt\t"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("trailing_tab.txt\t"));
}

/// Verifies handling of consecutive tabs.
#[test]
fn consecutive_tabs() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tab");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file\t\t\ttabs.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file\t\t\ttabs.txt"));
}

/// Verifies handling of tab and space mix.
#[test]
fn tab_and_space_mix() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tab");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file \t mixed.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file \t mixed.txt"));
}

/// Verifies handling of directory with tab.
#[test]
fn directory_with_tab() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tab");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new("dir\twith\ttab"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("dir\twith\ttab")));
    assert!(paths.contains(&PathBuf::from("dir\twith\ttab/inner.txt")));
}

// ============================================================================
// 6. Control Characters
// ============================================================================

/// Verifies handling of bell character (ASCII 7) in filename.
#[test]
fn bell_character_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("control");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::from_bytes(b"file\x07bell.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from(OsStr::from_bytes(b"file\x07bell.txt"))
    );
}

/// Verifies handling of backspace character (ASCII 8) in filename.
#[test]
fn backspace_character_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("control");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::from_bytes(b"file\x08backspace.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from(OsStr::from_bytes(b"file\x08backspace.txt"))
    );
}

/// Verifies handling of escape character (ASCII 27) in filename.
#[test]
fn escape_character_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("control");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::from_bytes(b"file\x1bescape.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from(OsStr::from_bytes(b"file\x1bescape.txt"))
    );
}

/// Verifies handling of form feed character (ASCII 12) in filename.
#[test]
fn form_feed_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("control");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::from_bytes(b"file\x0cformfeed.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from(OsStr::from_bytes(b"file\x0cformfeed.txt"))
    );
}

/// Verifies handling of vertical tab character (ASCII 11) in filename.
#[test]
fn vertical_tab_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("control");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::from_bytes(b"file\x0bvtab.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from(OsStr::from_bytes(b"file\x0bvtab.txt"))
    );
}

/// Verifies handling of multiple control characters.
#[test]
fn multiple_control_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("control");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::from_bytes(b"file\x01\x02\x03\x04\x05.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from(OsStr::from_bytes(b"file\x01\x02\x03\x04\x05.txt"))
    );
}

/// Verifies handling of DEL character (ASCII 127) in filename.
#[test]
fn del_character_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("control");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::from_bytes(b"file\x7fdel.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(
        paths[0],
        PathBuf::from(OsStr::from_bytes(b"file\x7fdel.txt"))
    );
}

/// Verifies handling of directory with control characters.
#[test]
fn directory_with_control_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("control");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::from_bytes(b"dir\x07control"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from(OsStr::from_bytes(b"dir\x07control"))));
}

// ============================================================================
// 7. Shell Metacharacters (*, ?, [, ])
// ============================================================================

/// Verifies handling of asterisk in filename.
#[test]
fn asterisk_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file*with*asterisk.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file*with*asterisk.txt"));
}

/// Verifies handling of question mark in filename.
#[test]
fn question_mark_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file?with?question.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file?with?question.txt"));
}

/// Verifies handling of square brackets in filename.
#[test]
fn square_brackets_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file[with]brackets.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file[with]brackets.txt"));
}

/// Verifies handling of glob pattern in filename.
#[test]
fn glob_pattern_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("*.txt"));
    create_test_file(&root, OsStr::new("file[0-9].txt"));
    create_test_file(&root, OsStr::new("file?.log"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 3);
    assert!(paths.contains(&PathBuf::from("*.txt")));
    assert!(paths.contains(&PathBuf::from("file[0-9].txt")));
    assert!(paths.contains(&PathBuf::from("file?.log")));
}

/// Verifies handling of combined metacharacters.
#[test]
fn combined_metacharacters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("*?[].txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("*?[].txt"));
}

/// Verifies handling of curly braces in filename (brace expansion).
#[test]
fn curly_braces_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file{a,b,c}.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file{a,b,c}.txt"));
}

/// Verifies handling of pipe character in filename.
#[test]
fn pipe_character_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file|pipe.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file|pipe.txt"));
}

/// Verifies handling of ampersand in filename.
#[test]
fn ampersand_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file&ampersand.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file&ampersand.txt"));
}

/// Verifies handling of semicolon in filename.
#[test]
fn semicolon_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file;semicolon.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file;semicolon.txt"));
}

/// Verifies handling of dollar sign in filename.
#[test]
fn dollar_sign_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file$dollar.txt"));
    create_test_file(&root, OsStr::new("$HOME.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("file$dollar.txt")));
    assert!(paths.contains(&PathBuf::from("$HOME.txt")));
}

/// Verifies handling of backtick in filename.
#[test]
fn backtick_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file`backtick`.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file`backtick`.txt"));
}

/// Verifies handling of parentheses in filename.
#[test]
fn parentheses_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file(with)(parens).txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file(with)(parens).txt"));
}

/// Verifies handling of redirection characters in filename.
#[test]
fn redirection_characters_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file<redirect>.txt"));
    create_test_file(&root, OsStr::new("file>redirect.txt"));
    create_test_file(&root, OsStr::new("file>>append.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 3);
    assert!(paths.contains(&PathBuf::from("file<redirect>.txt")));
    assert!(paths.contains(&PathBuf::from("file>redirect.txt")));
    assert!(paths.contains(&PathBuf::from("file>>append.txt")));
}

/// Verifies handling of directory with shell metacharacters.
#[test]
fn directory_with_shell_metacharacters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metachar");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new("dir*?[]"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("dir*?[]")));
    assert!(paths.contains(&PathBuf::from("dir*?[]/inner.txt")));
}

// ============================================================================
// 8. Leading/Trailing Dots
// ============================================================================

/// Verifies handling of single leading dot (hidden file).
#[test]
fn leading_single_dot() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dots");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new(".hidden"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from(".hidden"));
}

/// Verifies handling of double leading dots (but not ..).
#[test]
fn leading_double_dots_in_name() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dots");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("..hidden"));
    create_test_file(&root, OsStr::new("...hidden"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("..hidden")));
    assert!(paths.contains(&PathBuf::from("...hidden")));
}

/// Verifies handling of trailing dot.
#[test]
fn trailing_single_dot() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dots");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file."));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file."));
}

/// Verifies handling of multiple trailing dots.
#[test]
fn trailing_multiple_dots() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dots");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file.."));
    create_test_file(&root, OsStr::new("file..."));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("file..")));
    assert!(paths.contains(&PathBuf::from("file...")));
}

/// Verifies handling of dots-only filename.
#[test]
fn dots_only_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dots");
    fs::create_dir(&root).expect("create root");

    // Note: "." and ".." are reserved, but "..." is valid
    create_test_file(&root, OsStr::new("..."));
    create_test_file(&root, OsStr::new("...."));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("...")));
    assert!(paths.contains(&PathBuf::from("....")));
}

/// Verifies handling of hidden directory (leading dot).
#[test]
fn hidden_directory() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dots");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new(".hidden_dir"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from(".hidden_dir")));
    assert!(paths.contains(&PathBuf::from(".hidden_dir/inner.txt")));
}

/// Verifies multiple consecutive dots in middle.
#[test]
fn dots_in_middle() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dots");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file..name.txt"));
    create_test_file(&root, OsStr::new("file...name.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("file..name.txt")));
    assert!(paths.contains(&PathBuf::from("file...name.txt")));
}

// ============================================================================
// 9. Leading Dashes
// ============================================================================

/// Verifies handling of single leading dash.
#[test]
fn leading_single_dash() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dashes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("-file.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("-file.txt"));
}

/// Verifies handling of double leading dash.
#[test]
fn leading_double_dash() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dashes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("--file.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("--file.txt"));
}

/// Verifies handling of dash-only filename.
#[test]
fn dash_only_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dashes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("-"));
    create_test_file(&root, OsStr::new("--"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("-")));
    assert!(paths.contains(&PathBuf::from("--")));
}

/// Verifies handling of option-like filename.
#[test]
fn option_like_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dashes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("-r"));
    create_test_file(&root, OsStr::new("-rf"));
    create_test_file(&root, OsStr::new("--verbose"));
    create_test_file(&root, OsStr::new("--help"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from("-r")));
    assert!(paths.contains(&PathBuf::from("-rf")));
    assert!(paths.contains(&PathBuf::from("--verbose")));
    assert!(paths.contains(&PathBuf::from("--help")));
}

/// Verifies handling of trailing dash.
#[test]
fn trailing_dash() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dashes");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file-.txt"));
    create_test_file(&root, OsStr::new("file-"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("file-.txt")));
    assert!(paths.contains(&PathBuf::from("file-")));
}

/// Verifies handling of directory with leading dash.
#[test]
fn directory_with_leading_dash() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dashes");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new("-dir"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("-dir")));
    assert!(paths.contains(&PathBuf::from("-dir/inner.txt")));
}

// ============================================================================
// Combined Tests
// ============================================================================

/// Tests multiple special character categories in same directory.
#[test]
fn mixed_special_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixed");
    fs::create_dir(&root).expect("create root");

    // Create files with various special characters
    create_test_file(&root, OsStr::new("file with space.txt"));
    create_test_file(&root, OsStr::new("file'quote.txt"));
    create_test_file(&root, OsStr::new("file\\backslash.txt"));
    create_test_file(&root, OsStr::new("file\ttab.txt"));
    create_test_file(&root, OsStr::new("file*glob.txt"));
    create_test_file(&root, OsStr::new(".hidden"));
    create_test_file(&root, OsStr::new("-leading"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 7);
}

/// Tests deeply nested paths with special characters.
#[test]
fn deeply_nested_special_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("nested");
    fs::create_dir(&root).expect("create root");

    // Create nested structure with special chars at each level
    let level1 = create_test_dir(&root, OsStr::new("dir with space"));
    let level2 = create_test_dir(&level1, OsStr::new("dir'quote"));
    let level3 = create_test_dir(&level2, OsStr::new("dir*glob"));
    create_test_file(&level3, OsStr::new("deep file.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from(
        "dir with space/dir'quote/dir*glob/deep file.txt"
    )));
}

/// Tests entry metadata access with special character filenames.
#[test]
fn metadata_access_with_special_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("metadata");
    fs::create_dir(&root).expect("create root");

    let content = b"test content for metadata";
    let file_path = create_test_file(&root, OsStr::new("file with 'special\" chars*.txt"));
    fs::write(&file_path, content).expect("write content");

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    // Find our file entry
    let file_entry = entries
        .iter()
        .find(|e| !e.is_root())
        .expect("find file entry");

    assert!(file_entry.metadata().is_file());
    assert_eq!(file_entry.metadata().len(), content.len() as u64);
}

/// Tests file_name accessor with special character filenames.
#[test]
fn file_name_with_special_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("filename");
    fs::create_dir(&root).expect("create root");

    let special_name = OsStr::new("file with 'quotes\" and *globs*.txt");
    create_test_file(&root, special_name);

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let entries = collect_all_entries(walker);

    let file_entry = entries
        .iter()
        .find(|e| !e.is_root())
        .expect("find file entry");

    assert_eq!(file_entry.file_name(), Some(special_name));
}

/// Tests sorting with mixed special characters.
#[test]
fn sorting_with_special_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("sorting");
    fs::create_dir(&root).expect("create root");

    // Create files that might sort differently depending on implementation
    create_test_file(&root, OsStr::new(" space_first.txt"));
    create_test_file(&root, OsStr::new("!exclaim.txt"));
    create_test_file(&root, OsStr::new("-dash.txt"));
    create_test_file(&root, OsStr::new(".dot.txt"));
    create_test_file(&root, OsStr::new("0number.txt"));
    create_test_file(&root, OsStr::new("Auppercase.txt"));
    create_test_file(&root, OsStr::new("alowercase.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // Verify all files are present
    assert_eq!(paths.len(), 7);

    // Verify sorting is consistent (lexicographic)
    for i in 0..paths.len() - 1 {
        assert!(
            paths[i] < paths[i + 1],
            "paths should be sorted: {:?} should come before {:?}",
            paths[i],
            paths[i + 1]
        );
    }
}

// ============================================================================
// 10. Windows Reserved Names (Valid on Unix)
// ============================================================================

/// Verifies handling of Windows reserved device names (valid on Unix).
#[test]
fn windows_reserved_names() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("reserved");
    fs::create_dir(&root).expect("create root");

    // These are reserved on Windows but valid on Unix
    let reserved_names = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];

    for name in &reserved_names {
        create_test_file(&root, OsStr::new(name));
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), reserved_names.len());

    for name in &reserved_names {
        assert!(
            paths.contains(&PathBuf::from(*name)),
            "should contain reserved name: {name}"
        );
    }
}

/// Verifies handling of Windows reserved names with extensions.
#[test]
fn windows_reserved_names_with_extensions() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("reserved_ext");
    fs::create_dir(&root).expect("create root");

    // Windows also reserves names like CON.txt, NUL.dat, etc.
    let reserved_with_ext = [
        "CON.txt", "PRN.dat", "AUX.log", "NUL.bin", "COM1.cfg", "LPT1.doc",
    ];

    for name in &reserved_with_ext {
        create_test_file(&root, OsStr::new(name));
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), reserved_with_ext.len());

    for name in &reserved_with_ext {
        assert!(
            paths.contains(&PathBuf::from(*name)),
            "should contain reserved name with extension: {name}"
        );
    }
}

/// Verifies handling of Windows reserved names in lowercase.
#[test]
fn windows_reserved_names_lowercase() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("reserved_lower");
    fs::create_dir(&root).expect("create root");

    // Lowercase versions (Windows is case-insensitive, Unix is not)
    let lowercase_reserved = ["con", "prn", "aux", "nul", "com1", "lpt1"];

    for name in &lowercase_reserved {
        create_test_file(&root, OsStr::new(name));
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), lowercase_reserved.len());
}

/// Verifies handling of Windows reserved names as directory names.
#[test]
fn windows_reserved_names_as_directories() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("reserved_dirs");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new("CON"));
    create_test_file(&dir_path, OsStr::new("file.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("CON")));
    assert!(paths.contains(&PathBuf::from("CON/file.txt")));
}

// ============================================================================
// 11. Colon Character (Path Separator on Windows)
// ============================================================================

/// Verifies handling of colon in filename (valid on Unix, invalid on Windows).
#[test]
fn colon_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("colon");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file:with:colons.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("file:with:colons.txt"));
}

/// Verifies handling of colon at various positions.
#[test]
fn colon_positions() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("colon_pos");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new(":leading.txt"));
    create_test_file(&root, OsStr::new("trailing:.txt"));
    create_test_file(&root, OsStr::new("::double.txt"));
    create_test_file(&root, OsStr::new("time:12:30:45.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from(":leading.txt")));
    assert!(paths.contains(&PathBuf::from("trailing:.txt")));
    assert!(paths.contains(&PathBuf::from("::double.txt")));
    assert!(paths.contains(&PathBuf::from("time:12:30:45.txt")));
}

/// Verifies handling of directory with colon.
#[test]
fn directory_with_colon() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("colon_dir");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::new("dir:with:colon"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from("dir:with:colon")));
    assert!(paths.contains(&PathBuf::from("dir:with:colon/inner.txt")));
}

// ============================================================================
// 12. Additional Punctuation Characters
// ============================================================================

/// Verifies handling of hash/pound sign in filename.
#[test]
fn hash_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("hash");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("#hashtag.txt"));
    create_test_file(&root, OsStr::new("file#123.txt"));
    create_test_file(&root, OsStr::new("##double.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 3);
    assert!(paths.contains(&PathBuf::from("#hashtag.txt")));
    assert!(paths.contains(&PathBuf::from("file#123.txt")));
    assert!(paths.contains(&PathBuf::from("##double.txt")));
}

/// Verifies handling of at sign in filename.
#[test]
fn at_sign_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("at");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("@mention.txt"));
    create_test_file(&root, OsStr::new("email@domain.txt"));
    create_test_file(&root, OsStr::new("@@double.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 3);
    assert!(paths.contains(&PathBuf::from("@mention.txt")));
    assert!(paths.contains(&PathBuf::from("email@domain.txt")));
    assert!(paths.contains(&PathBuf::from("@@double.txt")));
}

/// Verifies handling of percent sign in filename.
#[test]
fn percent_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("percent");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("100%.txt"));
    create_test_file(&root, OsStr::new("%HOME%.txt"));
    create_test_file(&root, OsStr::new("file%20space.txt")); // URL encoding style

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 3);
    assert!(paths.contains(&PathBuf::from("100%.txt")));
    assert!(paths.contains(&PathBuf::from("%HOME%.txt")));
    assert!(paths.contains(&PathBuf::from("file%20space.txt")));
}

/// Verifies handling of caret in filename.
#[test]
fn caret_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("caret");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("x^2.txt"));
    create_test_file(&root, OsStr::new("^start.txt"));
    create_test_file(&root, OsStr::new("end^.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 3);
    assert!(paths.contains(&PathBuf::from("x^2.txt")));
    assert!(paths.contains(&PathBuf::from("^start.txt")));
    assert!(paths.contains(&PathBuf::from("end^.txt")));
}

/// Verifies handling of tilde in filename.
#[test]
fn tilde_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("tilde");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("~backup.txt"));
    create_test_file(&root, OsStr::new("file~.txt"));
    create_test_file(&root, OsStr::new("file.txt~"));
    create_test_file(&root, OsStr::new("~~double.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from("~backup.txt")));
    assert!(paths.contains(&PathBuf::from("file~.txt")));
    assert!(paths.contains(&PathBuf::from("file.txt~")));
    assert!(paths.contains(&PathBuf::from("~~double.txt")));
}

/// Verifies handling of plus sign in filename.
#[test]
fn plus_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("plus");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("+file.txt"));
    create_test_file(&root, OsStr::new("file+.txt"));
    create_test_file(&root, OsStr::new("1+1=2.txt"));
    create_test_file(&root, OsStr::new("++increment.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from("+file.txt")));
    assert!(paths.contains(&PathBuf::from("file+.txt")));
    assert!(paths.contains(&PathBuf::from("1+1=2.txt")));
    assert!(paths.contains(&PathBuf::from("++increment.txt")));
}

/// Verifies handling of equals sign in filename.
#[test]
fn equals_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("equals");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("a=b.txt"));
    create_test_file(&root, OsStr::new("=leading.txt"));
    create_test_file(&root, OsStr::new("trailing=.txt"));
    create_test_file(&root, OsStr::new("key=value.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from("a=b.txt")));
    assert!(paths.contains(&PathBuf::from("=leading.txt")));
    assert!(paths.contains(&PathBuf::from("trailing=.txt")));
    assert!(paths.contains(&PathBuf::from("key=value.txt")));
}

/// Verifies handling of exclamation mark in filename.
#[test]
fn exclamation_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("exclaim");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("!important.txt"));
    create_test_file(&root, OsStr::new("file!.txt"));
    create_test_file(&root, OsStr::new("hello!world.txt"));
    create_test_file(&root, OsStr::new("!!bang.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from("!important.txt")));
    assert!(paths.contains(&PathBuf::from("file!.txt")));
    assert!(paths.contains(&PathBuf::from("hello!world.txt")));
    assert!(paths.contains(&PathBuf::from("!!bang.txt")));
}

/// Verifies handling of comma in filename.
#[test]
fn comma_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("comma");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("file,name.txt"));
    create_test_file(&root, OsStr::new(",leading.txt"));
    create_test_file(&root, OsStr::new("trailing,.txt"));
    create_test_file(&root, OsStr::new("a,b,c.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
    assert!(paths.contains(&PathBuf::from("file,name.txt")));
    assert!(paths.contains(&PathBuf::from(",leading.txt")));
    assert!(paths.contains(&PathBuf::from("trailing,.txt")));
    assert!(paths.contains(&PathBuf::from("a,b,c.txt")));
}

// ============================================================================
// 13. Non-UTF8 Byte Sequences (Unix-specific)
// ============================================================================

/// Verifies handling of non-UTF8 byte sequences in filenames.
#[test]
fn non_utf8_bytes_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("nonutf8");
    fs::create_dir(&root).expect("create root");

    // Create filename with invalid UTF-8 bytes
    // 0x80-0xBF are continuation bytes that shouldn't appear standalone
    create_test_file(&root, OsStr::from_bytes(b"file\x80invalid.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\xff\xfe.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&PathBuf::from(OsStr::from_bytes(b"file\x80invalid.txt"))));
    assert!(paths.contains(&PathBuf::from(OsStr::from_bytes(b"file\xff\xfe.txt"))));
}

/// Verifies handling of high bytes (0x80-0xFF) in filenames.
#[test]
fn high_bytes_in_filename() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("highbytes");
    fs::create_dir(&root).expect("create root");

    // Various high-byte patterns
    create_test_file(&root, OsStr::from_bytes(b"file\x80.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\x90.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\xa0.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\xb0.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\xc0.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\xd0.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\xe0.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\xf0.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 8);
}

/// Verifies handling of directory with non-UTF8 name.
#[test]
fn directory_with_non_utf8_name() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("nonutf8_dir");
    fs::create_dir(&root).expect("create root");

    let dir_path = create_test_dir(&root, OsStr::from_bytes(b"dir\x80\x81"));
    create_test_file(&dir_path, OsStr::new("inner.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 2);
}

// ============================================================================
// 14. Null Byte Handling
// ============================================================================

// Note: Null bytes (0x00) cannot be in Unix filenames - the kernel rejects them.
// This is a fundamental limitation, not something we need to test.

// ============================================================================
// 15. All ASCII Printable Characters Combined
// ============================================================================

/// Verifies handling of all ASCII printable characters in one filename.
#[test]
fn all_ascii_printable_combined() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("allascii");
    fs::create_dir(&root).expect("create root");

    // All printable ASCII except / (path separator) and null
    // Space (32) through tilde (126), excluding / (47)
    let mut chars = Vec::new();
    for c in 32u8..127 {
        if c != b'/' {
            chars.push(c);
        }
    }
    let name = OsStr::from_bytes(&chars);
    create_test_file(&root, name);

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from(name));
}

/// Verifies each ASCII printable character in separate files.
#[test]
fn each_ascii_printable_separately() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("eachascii");
    fs::create_dir(&root).expect("create root");

    // Create file for each printable ASCII except / and null
    let mut expected_count = 0;
    for c in 32u8..127 {
        if c != b'/' {
            let name_bytes = [c, b'.', b't', b'x', b't'];
            create_test_file(&root, OsStr::from_bytes(&name_bytes));
            expected_count += 1;
        }
    }

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), expected_count);
}

// ============================================================================
// 16. Edge Cases with Path Components
// ============================================================================

/// Verifies handling of filename that looks like current directory.
#[test]
fn filename_looks_like_current_dir() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("dotlike");
    fs::create_dir(&root).expect("create root");

    // These are NOT . or .. but look similar
    create_test_file(&root, OsStr::new("..."));
    create_test_file(&root, OsStr::new("...."));
    create_test_file(&root, OsStr::new(". ."));
    create_test_file(&root, OsStr::new("..x"));
    create_test_file(&root, OsStr::new(".x."));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 5);
    assert!(paths.contains(&PathBuf::from("...")));
    assert!(paths.contains(&PathBuf::from("....")));
    assert!(paths.contains(&PathBuf::from(". .")));
    assert!(paths.contains(&PathBuf::from("..x")));
    assert!(paths.contains(&PathBuf::from(".x.")));
}

/// Verifies handling of very short filenames.
#[test]
fn very_short_filenames() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("short");
    fs::create_dir(&root).expect("create root");

    // Single character filenames
    create_test_file(&root, OsStr::new("a"));
    create_test_file(&root, OsStr::new("1"));
    create_test_file(&root, OsStr::new("_"));
    create_test_file(&root, OsStr::new("-"));
    create_test_file(&root, OsStr::new(" "));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 5);
    assert!(paths.contains(&PathBuf::from("a")));
    assert!(paths.contains(&PathBuf::from("1")));
    assert!(paths.contains(&PathBuf::from("_")));
    assert!(paths.contains(&PathBuf::from("-")));
    assert!(paths.contains(&PathBuf::from(" ")));
}

/// Verifies handling of files with only special characters.
#[test]
fn only_special_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("onlyspecial");
    fs::create_dir(&root).expect("create root");

    create_test_file(&root, OsStr::new("!!!"));
    create_test_file(&root, OsStr::new("@@@"));
    create_test_file(&root, OsStr::new("###"));
    create_test_file(&root, OsStr::new("$$$"));
    create_test_file(&root, OsStr::new("%%%"));
    create_test_file(&root, OsStr::new("^^^"));
    create_test_file(&root, OsStr::new("&&&"));
    create_test_file(&root, OsStr::new("***"));
    create_test_file(&root, OsStr::new("((("));
    create_test_file(&root, OsStr::new(")))"));
    create_test_file(&root, OsStr::new("___"));
    create_test_file(&root, OsStr::new("+++"));
    create_test_file(&root, OsStr::new("==="));
    create_test_file(&root, OsStr::new("~~~"));
    create_test_file(&root, OsStr::new("```"));
    create_test_file(&root, OsStr::new("[[["));
    create_test_file(&root, OsStr::new("]]]"));
    create_test_file(&root, OsStr::new("{{{"));
    create_test_file(&root, OsStr::new("}}}"));
    create_test_file(&root, OsStr::new("|||"));
    create_test_file(&root, OsStr::new(":::"));
    create_test_file(&root, OsStr::new(";;;"));
    create_test_file(&root, OsStr::new("'''"));
    create_test_file(&root, OsStr::new("\"\"\""));
    create_test_file(&root, OsStr::new(",,,"));
    create_test_file(&root, OsStr::new("<<<"));
    create_test_file(&root, OsStr::new(">>>"));
    create_test_file(&root, OsStr::new("???"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 28);
}

// ============================================================================
// 17. Mixed Whitespace Characters
// ============================================================================

/// Verifies handling of various whitespace characters.
#[test]
fn various_whitespace_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("whitespace");
    fs::create_dir(&root).expect("create root");

    // Different whitespace: space, tab, vertical tab, form feed
    create_test_file(&root, OsStr::new("file with space.txt"));
    create_test_file(&root, OsStr::new("file\twith\ttab.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\x0bvtab.txt"));
    create_test_file(&root, OsStr::from_bytes(b"file\x0cformfeed.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 4);
}

/// Verifies handling of mixed whitespace in single filename.
#[test]
fn mixed_whitespace_single_file() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("mixedws");
    fs::create_dir(&root).expect("create root");

    // Space, tab, space, tab pattern
    create_test_file(&root, OsStr::new("a b\tc d\te.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0], PathBuf::from("a b\tc d\te.txt"));
}

// ============================================================================
// 18. Comprehensive Shell Injection Prevention
// ============================================================================

/// Verifies handling of filenames that could cause shell injection.
#[test]
fn shell_injection_patterns() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("injection");
    fs::create_dir(&root).expect("create root");

    // Patterns that could cause issues if improperly escaped
    // Note: Can't use / in filenames as it's a path separator on Unix
    create_test_file(&root, OsStr::new("; rm -rf ~.txt"));
    create_test_file(&root, OsStr::new("| cat etc_passwd.txt"));
    create_test_file(&root, OsStr::new("$(whoami).txt"));
    create_test_file(&root, OsStr::new("`whoami`.txt"));
    create_test_file(&root, OsStr::new("&& echo pwned.txt"));
    create_test_file(&root, OsStr::new("|| true.txt"));
    create_test_file(&root, OsStr::new("> dev_null.txt"));
    create_test_file(&root, OsStr::new("< dev_zero.txt"));
    create_test_file(&root, OsStr::new("2>&1.txt"));
    create_test_file(&root, OsStr::new("$((1+1)).txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 10);

    // Verify exact names are preserved
    assert!(paths.contains(&PathBuf::from("; rm -rf ~.txt")));
    assert!(paths.contains(&PathBuf::from("| cat etc_passwd.txt")));
    assert!(paths.contains(&PathBuf::from("$(whoami).txt")));
    assert!(paths.contains(&PathBuf::from("`whoami`.txt")));
}

/// Verifies handling of filenames with escape sequences.
#[test]
fn escape_sequence_patterns() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("escape");
    fs::create_dir(&root).expect("create root");

    // Literal backslash sequences (not actual escapes)
    create_test_file(&root, OsStr::new("file\\nname.txt"));
    create_test_file(&root, OsStr::new("file\\tname.txt"));
    create_test_file(&root, OsStr::new("file\\rname.txt"));
    create_test_file(&root, OsStr::new("file\\\\name.txt"));
    create_test_file(&root, OsStr::new("file\\'name.txt"));
    create_test_file(&root, OsStr::new("file\\\"name.txt"));
    create_test_file(&root, OsStr::new("file\\0name.txt"));

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 7);
}

// ============================================================================
// 19. Unicode Normalization Edge Cases
// ============================================================================

/// Verifies handling of Unicode look-alikes.
#[test]
fn unicode_lookalikes() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("lookalikes");
    fs::create_dir(&root).expect("create root");

    // Latin 'a' vs Cyrillic 'a' (U+0430) - they look the same!
    create_test_file(&root, OsStr::new("a.txt")); // Latin
    create_test_file(&root, OsStr::new("\u{0430}.txt")); // Cyrillic

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    // On case-sensitive filesystems, both should exist as separate files
    assert_eq!(paths.len(), 2);
}

/// Verifies handling of fullwidth characters.
#[test]
fn fullwidth_characters() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let root = temp.path().join("fullwidth");
    fs::create_dir(&root).expect("create root");

    // Fullwidth Latin letters (used in CJK contexts)
    create_test_file(&root, OsStr::new("\u{FF21}.txt")); // Fullwidth A
    create_test_file(&root, OsStr::new("\u{FF22}.txt")); // Fullwidth B
    create_test_file(&root, OsStr::new("\u{FF23}.txt")); // Fullwidth C

    let walker = FileListBuilder::new(&root).build().expect("build walker");
    let paths = collect_relative_paths(walker);

    assert_eq!(paths.len(), 3);
}
