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
