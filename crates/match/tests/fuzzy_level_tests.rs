//! Integration tests for fuzzy matching level support.
//!
//! These tests verify that the fuzzy matcher correctly implements level 1
//! (--fuzzy) and level 2 (-yy) behavior, matching upstream rsync's semantics.

use matching::{FUZZY_LEVEL_1, FUZZY_LEVEL_2, FuzzyMatcher};
use std::ffi::OsStr;
use std::fs;
use tempfile::TempDir;

/// Verifies that level 1 fuzzy only searches the destination directory.
#[test]
fn level_1_searches_only_dest_directory() {
    let dest_dir = TempDir::new().expect("create dest dir");
    let ref_dir = TempDir::new().expect("create ref dir");

    // Put candidates in both directories
    fs::write(dest_dir.path().join("file_v1.txt"), "dest version").expect("write dest");
    fs::write(ref_dir.path().join("file_v0.txt"), "ref version").expect("write ref");

    // Level 1 matcher with reference directory configured
    let matcher =
        FuzzyMatcher::with_level(1).with_fuzzy_basis_dirs(vec![ref_dir.path().to_path_buf()]);

    let result = matcher.find_fuzzy_basis(OsStr::new("file_v2.txt"), dest_dir.path(), 100);

    // Should find the file in dest_dir, not ref_dir (level 1 doesn't search ref dirs)
    assert!(result.is_some());
    let matched = result.unwrap();
    assert!(
        matched.path.starts_with(dest_dir.path()),
        "Level 1 should only search dest directory"
    );
}

/// Verifies that level 2 fuzzy searches both dest and reference directories.
#[test]
fn level_2_searches_dest_and_reference_dirs() {
    let dest_dir = TempDir::new().expect("create dest dir");
    let ref_dir1 = TempDir::new().expect("create ref dir 1");
    let ref_dir2 = TempDir::new().expect("create ref dir 2");

    // Put candidates in reference directories only
    fs::write(ref_dir1.path().join("app_v1.0.tar.gz"), "version 1.0").expect("write ref1");
    fs::write(ref_dir2.path().join("app_v1.1.tar.gz"), "version 1.1").expect("write ref2");

    // Level 2 matcher
    let matcher = FuzzyMatcher::with_level(2).with_fuzzy_basis_dirs(vec![
        ref_dir1.path().to_path_buf(),
        ref_dir2.path().to_path_buf(),
    ]);

    let result = matcher.find_fuzzy_basis(OsStr::new("app_v1.2.tar.gz"), dest_dir.path(), 100);

    // Should find a file in one of the reference directories
    assert!(result.is_some());
    let matched = result.unwrap();
    assert!(
        matched.path.starts_with(ref_dir1.path()) || matched.path.starts_with(ref_dir2.path()),
        "Level 2 should search reference directories"
    );
}

/// Verifies that level 2 prefers better matches across all directories.
#[test]
fn level_2_chooses_best_match_across_all_dirs() {
    let dest_dir = TempDir::new().expect("create dest dir");
    let ref_dir = TempDir::new().expect("create ref dir");

    // Dest has a poor match
    fs::write(dest_dir.path().join("other_file.dat"), "x".repeat(1000)).expect("write dest");

    // Ref has a better match (same name pattern)
    fs::write(ref_dir.path().join("data_2023.csv"), "x".repeat(900)).expect("write ref");

    // Level 2 matcher
    let matcher =
        FuzzyMatcher::with_level(2).with_fuzzy_basis_dirs(vec![ref_dir.path().to_path_buf()]);

    let result = matcher.find_fuzzy_basis(OsStr::new("data_2024.csv"), dest_dir.path(), 1000);

    // Should choose the better match from ref_dir
    assert!(result.is_some());
    let matched = result.unwrap();
    assert!(
        matched.path.starts_with(ref_dir.path()),
        "Should choose better match from reference directory"
    );
    assert!(matched.score > 50, "Should have a good score");
}

/// Verifies that default matcher uses level 1.
#[test]
fn default_matcher_is_level_1() {
    let matcher = FuzzyMatcher::new();
    assert_eq!(matcher.fuzzy_level(), FUZZY_LEVEL_1);
}

/// Verifies that with_level constructor works correctly.
#[test]
fn with_level_constructor() {
    let matcher1 = FuzzyMatcher::with_level(1);
    assert_eq!(matcher1.fuzzy_level(), 1);

    let matcher2 = FuzzyMatcher::with_level(2);
    assert_eq!(matcher2.fuzzy_level(), 2);
}

/// Verifies that level 2 without configured basis dirs acts like level 1.
#[test]
fn level_2_without_basis_dirs_acts_like_level_1() {
    let dest_dir = TempDir::new().expect("create dest dir");

    fs::write(dest_dir.path().join("test_v1.txt"), "data").expect("write");

    // Level 2 but no basis dirs configured
    let matcher = FuzzyMatcher::with_level(2);

    let result = matcher.find_fuzzy_basis(OsStr::new("test_v2.txt"), dest_dir.path(), 10);

    // Should still find the file in dest_dir
    assert!(result.is_some());
}

/// Verifies real-world scenario: renamed versioned files.
#[test]
fn real_world_versioned_file_rename() {
    let dest_dir = TempDir::new().expect("create dest dir");

    // Old version exists
    fs::write(dest_dir.path().join("myapp-1.2.3.tar.gz"), "x".repeat(5000))
        .expect("write old version");

    let matcher = FuzzyMatcher::new();
    let result = matcher.find_fuzzy_basis(OsStr::new("myapp-1.2.4.tar.gz"), dest_dir.path(), 5100);

    assert!(result.is_some(), "Should find old version as basis");
    let matched = result.unwrap();
    assert!(
        matched
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("1.2.3"),
        "Should match old version"
    );
    assert!(matched.score > 100, "Should have high similarity score");
}

/// Verifies real-world scenario: date-stamped backups.
#[test]
fn real_world_dated_backup_files() {
    let dest_dir = TempDir::new().expect("create dest dir");

    // Previous backups exist
    fs::write(
        dest_dir.path().join("backup_2024-01-15.tar"),
        "x".repeat(10000),
    )
    .expect("write backup");

    let matcher = FuzzyMatcher::new();
    let result =
        matcher.find_fuzzy_basis(OsStr::new("backup_2024-01-22.tar"), dest_dir.path(), 10500);

    assert!(result.is_some(), "Should find previous backup as basis");
    let matched = result.unwrap();
    assert!(
        matched.score > 150,
        "Date-stamped backups should score high"
    );
}

/// Verifies that fuzzy matching respects minimum score at each level.
#[test]
fn minimum_score_respected_at_all_levels() {
    let dest_dir = TempDir::new().expect("create dest dir");
    let ref_dir = TempDir::new().expect("create ref dir");

    // Poor match in dest
    fs::write(dest_dir.path().join("abc.txt"), "data1").expect("write dest");

    // Poor match in ref
    fs::write(ref_dir.path().join("xyz.txt"), "data2").expect("write ref");

    // Level 2 matcher with high threshold
    let matcher = FuzzyMatcher::with_level(2)
        .with_min_score(1000)
        .with_fuzzy_basis_dirs(vec![ref_dir.path().to_path_buf()]);

    let result = matcher.find_fuzzy_basis(OsStr::new("target.dat"), dest_dir.path(), 100);

    // Should find nothing due to high threshold
    assert!(
        result.is_none(),
        "High threshold should reject poor matches"
    );
}

/// Verifies fuzzy level constants are defined correctly.
#[test]
fn fuzzy_level_constants() {
    assert_eq!(FUZZY_LEVEL_1, 1, "Level 1 constant");
    assert_eq!(FUZZY_LEVEL_2, 2, "Level 2 constant");
}
