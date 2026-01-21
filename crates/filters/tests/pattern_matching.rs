//! Integration tests for include/exclude pattern matching.
//!
//! These tests verify that the filter pattern matching behavior mirrors
//! upstream rsync 3.4.1's exclude.c implementation. Tests cover:
//! - Wildcard patterns (*, ?, **)
//! - Character class patterns ([])
//! - Anchored vs unanchored patterns
//! - Directory-only patterns
//! - Pattern precedence and rule ordering

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Basic Wildcard Tests (*, ?)
// ============================================================================

/// Verifies that `*` matches any sequence of characters except `/`.
///
/// From rsync man page: "a `*` matches any path component, but it stops
/// at slashes."
#[test]
fn single_star_matches_filename_characters() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.txt")]).unwrap();

    // Should match .txt extension at any depth
    assert!(!set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("README.txt"), false));
    assert!(!set.allows(Path::new("a.txt"), false));

    // Should not match different extensions
    assert!(set.allows(Path::new("file.md"), false));
    assert!(set.allows(Path::new("file.txtx"), false));
    assert!(set.allows(Path::new("file.tx"), false));
}

/// Verifies `*` does not match across path separators.
#[test]
fn single_star_does_not_match_path_separator() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.txt")]).unwrap();

    // Should match at any depth due to implicit **/ prefix for unanchored patterns
    assert!(!set.allows(Path::new("dir/file.txt"), false));
    assert!(!set.allows(Path::new("a/b/c/file.txt"), false));
}

/// Verifies `*` at end of pattern.
#[test]
fn single_star_at_end_matches_any_suffix() {
    let set = FilterSet::from_rules([FilterRule::exclude("file*")]).unwrap();

    assert!(!set.allows(Path::new("file"), false));
    assert!(!set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("filename"), false));
    assert!(!set.allows(Path::new("file123"), false));

    // Should not match different prefixes
    assert!(set.allows(Path::new("myfile"), false));
    assert!(set.allows(Path::new("afile"), false));
}

/// Verifies `?` matches exactly one character.
///
/// From rsync man page: "a `?` matches any character except a slash."
#[test]
fn question_mark_matches_single_character() {
    let set = FilterSet::from_rules([FilterRule::exclude("file?.txt")]).unwrap();

    assert!(!set.allows(Path::new("file1.txt"), false));
    assert!(!set.allows(Path::new("fileA.txt"), false));
    assert!(!set.allows(Path::new("file_.txt"), false));

    // Should not match zero or multiple characters
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(set.allows(Path::new("file12.txt"), false));
}

/// Verifies multiple `?` in a pattern.
#[test]
fn multiple_question_marks() {
    let set = FilterSet::from_rules([FilterRule::exclude("???.log")]).unwrap();

    assert!(!set.allows(Path::new("app.log"), false));
    assert!(!set.allows(Path::new("123.log"), false));

    // Wrong length
    assert!(set.allows(Path::new("ap.log"), false));
    assert!(set.allows(Path::new("apps.log"), false));
}

// ============================================================================
// Double-Star Wildcard Tests (**)
// ============================================================================

/// Verifies `**` matches any path including slashes.
///
/// From rsync man page: "a `**` matches anything, including slashes."
#[test]
fn double_star_matches_path_separators() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/build")]).unwrap();

    assert!(!set.allows(Path::new("build"), false));
    assert!(!set.allows(Path::new("src/build"), false));
    assert!(!set.allows(Path::new("a/b/c/build"), false));
}

/// Verifies `**` at end matches all descendants.
#[test]
fn double_star_at_end_matches_all_descendants() {
    let set = FilterSet::from_rules([FilterRule::exclude("cache/**")]).unwrap();

    assert!(!set.allows(Path::new("cache/file.txt"), false));
    assert!(!set.allows(Path::new("cache/subdir/file.txt"), false));
    assert!(!set.allows(Path::new("cache/a/b/c/deep.txt"), false));

    // Parent directory not matched by /**
    assert!(set.allows(Path::new("cache"), true));
}

/// Verifies `**` in the middle of a pattern.
#[test]
fn double_star_in_middle() {
    let set = FilterSet::from_rules([FilterRule::exclude("src/**/test.rs")]).unwrap();

    assert!(!set.allows(Path::new("src/test.rs"), false));
    assert!(!set.allows(Path::new("src/module/test.rs"), false));
    assert!(!set.allows(Path::new("src/a/b/c/test.rs"), false));

    // Different base path not matched
    assert!(set.allows(Path::new("lib/test.rs"), false));
}

/// Verifies leading `**/` matches at root or any depth.
#[test]
fn leading_double_star_slash() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/node_modules")]).unwrap();

    assert!(!set.allows(Path::new("node_modules"), true));
    assert!(!set.allows(Path::new("packages/node_modules"), true));
    assert!(!set.allows(Path::new("deep/nested/node_modules"), true));
}

// ============================================================================
// Character Class Tests ([])
// ============================================================================

/// Verifies character classes match any single character in the set.
#[test]
fn character_class_basic() {
    let set = FilterSet::from_rules([FilterRule::exclude("file[123].txt")]).unwrap();

    assert!(!set.allows(Path::new("file1.txt"), false));
    assert!(!set.allows(Path::new("file2.txt"), false));
    assert!(!set.allows(Path::new("file3.txt"), false));

    // Not in class
    assert!(set.allows(Path::new("file4.txt"), false));
    assert!(set.allows(Path::new("filea.txt"), false));
}

/// Verifies character ranges in character classes.
#[test]
fn character_class_range() {
    let set = FilterSet::from_rules([FilterRule::exclude("file[a-z].txt")]).unwrap();

    assert!(!set.allows(Path::new("filea.txt"), false));
    assert!(!set.allows(Path::new("filez.txt"), false));
    assert!(!set.allows(Path::new("filem.txt"), false));

    // Uppercase not in range
    assert!(set.allows(Path::new("fileA.txt"), false));
    // Digit not in range
    assert!(set.allows(Path::new("file1.txt"), false));
}

/// Verifies negated character classes with `!` or `^`.
#[test]
fn character_class_negation() {
    let set = FilterSet::from_rules([FilterRule::exclude("file[!0-9].txt")]).unwrap();

    // Non-digits should be excluded
    assert!(!set.allows(Path::new("filea.txt"), false));
    assert!(!set.allows(Path::new("fileX.txt"), false));

    // Digits should be allowed
    assert!(set.allows(Path::new("file0.txt"), false));
    assert!(set.allows(Path::new("file9.txt"), false));
}

/// Verifies multiple character classes in a pattern.
#[test]
fn multiple_character_classes() {
    let set = FilterSet::from_rules([FilterRule::exclude("[a-z][0-9].dat")]).unwrap();

    assert!(!set.allows(Path::new("a1.dat"), false));
    assert!(!set.allows(Path::new("z9.dat"), false));
    assert!(!set.allows(Path::new("m5.dat"), false));

    // Wrong pattern
    assert!(set.allows(Path::new("1a.dat"), false));
    assert!(set.allows(Path::new("aa.dat"), false));
}

// ============================================================================
// Anchored Pattern Tests (leading /)
// ============================================================================

/// Verifies anchored patterns only match at root.
///
/// From rsync man page: "if the pattern starts with a / then it is
/// anchored to a particular spot in the hierarchy of files."
#[test]
fn anchored_pattern_matches_only_at_root() {
    let set = FilterSet::from_rules([FilterRule::exclude("/config.ini")]).unwrap();

    // Should match at root only
    assert!(!set.allows(Path::new("config.ini"), false));

    // Should not match at depth
    assert!(set.allows(Path::new("dir/config.ini"), false));
    assert!(set.allows(Path::new("a/b/config.ini"), false));
}

/// Verifies anchored path patterns.
#[test]
fn anchored_path_pattern() {
    let set = FilterSet::from_rules([FilterRule::exclude("/src/generated")]).unwrap();

    assert!(!set.allows(Path::new("src/generated"), false));
    assert!(!set.allows(Path::new("src/generated/file.rs"), false));

    // Different path
    assert!(set.allows(Path::new("lib/src/generated"), false));
    assert!(set.allows(Path::new("other/generated"), false));
}

/// Verifies unanchored patterns match at any depth.
#[test]
fn unanchored_pattern_matches_at_any_depth() {
    let set = FilterSet::from_rules([FilterRule::exclude("temp.dat")]).unwrap();

    assert!(!set.allows(Path::new("temp.dat"), false));
    assert!(!set.allows(Path::new("dir/temp.dat"), false));
    assert!(!set.allows(Path::new("a/b/c/temp.dat"), false));
}

/// Verifies patterns with internal slashes become anchored.
///
/// From rsync man page: "if the pattern contains a / (not counting a
/// trailing /) or a ** then it is matched against the full pathname."
#[test]
fn pattern_with_slash_is_implicitly_anchored() {
    let set = FilterSet::from_rules([FilterRule::exclude("src/temp")]).unwrap();

    // Matches the path component sequence anywhere
    assert!(!set.allows(Path::new("src/temp"), false));
    assert!(!set.allows(Path::new("project/src/temp"), false));

    // Different path doesn't match
    assert!(set.allows(Path::new("lib/temp"), false));
}

// ============================================================================
// Directory-Only Pattern Tests (trailing /)
// ============================================================================

/// Verifies directory-only patterns only match directories.
///
/// From rsync man page: "if the pattern ends with a / then it will only
/// match a directory, not a regular file, symlink, or device."
#[test]
fn directory_only_pattern_matches_directories() {
    let set = FilterSet::from_rules([FilterRule::exclude("build/")]).unwrap();

    // Should match directory
    assert!(!set.allows(Path::new("build"), true));

    // Should not match file with same name
    assert!(set.allows(Path::new("build"), false));
}

/// Verifies directory pattern excludes contents.
#[test]
fn directory_pattern_excludes_contents() {
    let set = FilterSet::from_rules([FilterRule::exclude("node_modules/")]).unwrap();

    // Directory itself
    assert!(!set.allows(Path::new("node_modules"), true));

    // Directory contents
    assert!(!set.allows(Path::new("node_modules/package.json"), false));
    assert!(!set.allows(Path::new("node_modules/lodash/index.js"), false));
}

/// Verifies nested directory patterns.
#[test]
fn nested_directory_pattern() {
    let set = FilterSet::from_rules([FilterRule::exclude("packages/*/node_modules/")]).unwrap();

    assert!(!set.allows(Path::new("packages/app/node_modules"), true));
    assert!(!set.allows(Path::new("packages/lib/node_modules"), true));
    assert!(!set.allows(Path::new("packages/app/node_modules/pkg"), false));

    // Root node_modules not matched
    assert!(set.allows(Path::new("node_modules"), true));
}

// ============================================================================
// Rule Ordering and Precedence Tests
// ============================================================================

/// Verifies that first matching rule wins.
///
/// From rsync man page: "the include/exclude rules are checked in order
/// and the first matching rule is used."
#[test]
fn first_matching_rule_wins() {
    // With first-match-wins, specific include must come before general exclude
    let rules = [
        FilterRule::include("important.txt"),
        FilterRule::exclude("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Specifically included file is allowed (first rule)
    assert!(set.allows(Path::new("important.txt"), false));

    // Other .txt files are excluded (second rule)
    assert!(!set.allows(Path::new("other.txt"), false));
    assert!(!set.allows(Path::new("readme.txt"), false));
}

/// Verifies multiple include/exclude interactions.
#[test]
fn complex_rule_ordering() {
    // With first-match-wins, most specific rules come first
    let rules = [
        FilterRule::include("test_utils.rs"), // Most specific first
        FilterRule::exclude("test_*.rs"),     // Then test pattern
        FilterRule::include("*.rs"),          // Then general .rs include
        FilterRule::exclude("*"),             // Finally catch-all exclude
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // test_utils.rs is specifically included (rule 1)
    assert!(set.allows(Path::new("test_utils.rs"), false));

    // Other test_*.rs files excluded (rule 2)
    assert!(!set.allows(Path::new("test_main.rs"), false));

    // Regular .rs files allowed (rule 3)
    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("lib.rs"), false));

    // Non-.rs files excluded (rule 4)
    assert!(!set.allows(Path::new("Cargo.toml"), false));
}

/// Verifies that include can create exception for directory exclusion.
#[test]
fn include_creates_exception_for_excluded_directory() {
    // With first-match-wins, include must come before exclude
    let rules = [
        FilterRule::include("target/doc/**"), // Include doc contents first
        FilterRule::exclude("target/"),       // Then exclude target
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // target/doc contents are specifically included (rule 1)
    assert!(set.allows(Path::new("target/doc/index.html"), false));

    // Other target contents excluded (rule 2)
    assert!(!set.allows(Path::new("target/debug"), true));
    assert!(!set.allows(Path::new("target/release/binary"), false));
}

// ============================================================================
// Default Behavior Tests
// ============================================================================

/// Verifies that paths with no matching rules are included by default.
#[test]
fn no_matching_rule_defaults_to_include() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();

    // Non-matching files are allowed
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(set.allows(Path::new("document.pdf"), false));
}

/// Verifies empty filter set allows everything.
#[test]
fn empty_filter_set_allows_all() {
    let set = FilterSet::from_rules(Vec::<FilterRule>::new()).unwrap();

    assert!(set.allows(Path::new("anything"), false));
    assert!(set.allows(Path::new("deep/nested/path"), false));
    assert!(set.allows(Path::new("file.any"), false));
}

// ============================================================================
// Edge Cases and Complex Patterns
// ============================================================================

/// Verifies pattern with multiple wildcards.
#[test]
fn multiple_wildcards_in_pattern() {
    let set = FilterSet::from_rules([FilterRule::exclude("*_test_*.rs")]).unwrap();

    assert!(!set.allows(Path::new("unit_test_foo.rs"), false));
    assert!(!set.allows(Path::new("my_test_utils.rs"), false));

    // Single underscore doesn't match
    assert!(set.allows(Path::new("test_foo.rs"), false));
    assert!(set.allows(Path::new("foo_test.rs"), false));
}

/// Verifies patterns with dots.
#[test]
fn patterns_with_dots() {
    let set = FilterSet::from_rules([FilterRule::exclude(".*")]).unwrap();

    // Hidden files
    assert!(!set.allows(Path::new(".gitignore"), false));
    assert!(!set.allows(Path::new(".env"), false));

    // Regular files not matched
    assert!(set.allows(Path::new("file.txt"), false));
}

/// Verifies escaped special characters.
#[test]
fn escaped_special_characters() {
    let set = FilterSet::from_rules([FilterRule::exclude("file\\[1\\].txt")]).unwrap();

    // Literal brackets
    assert!(!set.allows(Path::new("file[1].txt"), false));

    // Character class interpretation should not occur
    assert!(set.allows(Path::new("file1.txt"), false));
}

/// Verifies escaped question mark.
#[test]
fn escaped_question_mark() {
    let set = FilterSet::from_rules([FilterRule::exclude("file\\?.txt")]).unwrap();

    // Literal question mark
    assert!(!set.allows(Path::new("file?.txt"), false));

    // Single char wildcard should not match
    assert!(set.allows(Path::new("file1.txt"), false));
}

/// Verifies very long patterns.
#[test]
fn long_pattern() {
    let long_name = "a".repeat(200);
    let pattern = format!("{long_name}.txt");
    let set = FilterSet::from_rules([FilterRule::exclude(&pattern)]).unwrap();

    assert!(!set.allows(Path::new(&format!("{long_name}.txt")), false));
}

/// Verifies patterns that could match empty strings.
#[test]
fn pattern_with_optional_content() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.backup.*")]).unwrap();

    assert!(!set.allows(Path::new("file.backup.txt"), false));
    assert!(!set.allows(Path::new("doc.backup.bak"), false));

    // Missing middle part
    assert!(set.allows(Path::new("file.txt"), false));
}

// ============================================================================
// Path Normalization Tests
// ============================================================================

/// Verifies paths with trailing slashes are handled.
#[test]
fn path_with_trailing_components() {
    let set = FilterSet::from_rules([FilterRule::exclude("build/")]).unwrap();

    // With various path forms
    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("build/output"), false));
}

/// Verifies case sensitivity (rsync is case-sensitive by default).
#[test]
fn case_sensitive_matching() {
    let set = FilterSet::from_rules([FilterRule::exclude("README.md")]).unwrap();

    assert!(!set.allows(Path::new("README.md"), false));

    // Different case should not match
    assert!(set.allows(Path::new("readme.md"), false));
    assert!(set.allows(Path::new("Readme.md"), false));
    assert!(set.allows(Path::new("README.MD"), false));
}

// ============================================================================
// Interaction with Directory Flag Tests
// ============================================================================

/// Verifies is_dir parameter affects directory-only rules.
#[test]
fn is_dir_parameter_affects_directory_rules() {
    let set = FilterSet::from_rules([FilterRule::exclude("output/")]).unwrap();

    // Directory rule matches directory
    assert!(!set.allows(Path::new("output"), true));

    // Directory rule does not match file
    assert!(set.allows(Path::new("output"), false));
}

/// Verifies non-directory rules match regardless of is_dir.
#[test]
fn non_directory_rules_match_any_type() {
    let set = FilterSet::from_rules([FilterRule::exclude("temp")]).unwrap();

    // Matches both files and directories
    assert!(!set.allows(Path::new("temp"), false));
    assert!(!set.allows(Path::new("temp"), true));
}
