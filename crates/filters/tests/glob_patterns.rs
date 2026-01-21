//! Integration tests for glob pattern matching.
//!
//! These tests comprehensively cover the glob pattern syntax supported by
//! the filters crate, including wildcards, character classes, and the
//! special `**` recursive wildcard. Patterns follow rsync's glob semantics.
//!
//! Reference: rsync 3.4.1 exclude.c lines 236-250 for wildcard flag handling.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Single Star Wildcard Tests (*)
// ============================================================================

/// Verifies `*` matches any filename characters.
#[test]
fn star_matches_any_filename() {
    let set = FilterSet::from_rules([FilterRule::exclude("file*")]).unwrap();

    assert!(!set.allows(Path::new("file"), false));
    assert!(!set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("filename"), false));
    assert!(!set.allows(Path::new("file123"), false));
    assert!(!set.allows(Path::new("file.tar.gz"), false));
}

/// Verifies `*` at start of pattern.
#[test]
fn star_at_start() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.txt")]).unwrap();

    assert!(!set.allows(Path::new("readme.txt"), false));
    assert!(!set.allows(Path::new("a.txt"), false));
    assert!(!set.allows(Path::new(".txt"), false)); // Just extension
}

/// Verifies `*` in middle of pattern.
#[test]
fn star_in_middle() {
    let set = FilterSet::from_rules([FilterRule::exclude("file*.log")]).unwrap();

    assert!(!set.allows(Path::new("file.log"), false));
    assert!(!set.allows(Path::new("file1.log"), false));
    assert!(!set.allows(Path::new("file_app.log"), false));
    assert!(set.allows(Path::new("file.txt"), false));
}

/// Verifies multiple `*` in pattern.
#[test]
fn multiple_stars() {
    let set = FilterSet::from_rules([FilterRule::exclude("*_*_*.txt")]).unwrap();

    assert!(!set.allows(Path::new("a_b_c.txt"), false));
    assert!(!set.allows(Path::new("foo_bar_baz.txt"), false));
    assert!(set.allows(Path::new("a_b.txt"), false)); // Only one underscore
}

/// Verifies `*` does not match path separator.
#[test]
fn star_does_not_match_slash() {
    // Anchored pattern so we can test slash non-matching
    let set = FilterSet::from_rules([FilterRule::exclude("/src/*.rs")]).unwrap();

    assert!(!set.allows(Path::new("src/main.rs"), false));
    assert!(!set.allows(Path::new("src/lib.rs"), false));

    // Does not match nested files (star doesn't cross slash)
    // But the pattern is anchored and uses implicit ** for unanchored
    // With anchored /src/*.rs, it should only match direct children
    assert!(set.allows(Path::new("src/module/mod.rs"), false));
}

// ============================================================================
// Question Mark Wildcard Tests (?)
// ============================================================================

/// Verifies `?` matches exactly one character.
#[test]
fn question_matches_single_char() {
    let set = FilterSet::from_rules([FilterRule::exclude("file?.txt")]).unwrap();

    assert!(!set.allows(Path::new("file1.txt"), false));
    assert!(!set.allows(Path::new("fileA.txt"), false));
    assert!(!set.allows(Path::new("file_.txt"), false));

    // Does not match zero characters
    assert!(set.allows(Path::new("file.txt"), false));

    // Does not match two characters
    assert!(set.allows(Path::new("file12.txt"), false));
}

/// Verifies multiple `?` in pattern.
#[test]
fn multiple_questions() {
    let set = FilterSet::from_rules([FilterRule::exclude("???")]).unwrap();

    assert!(!set.allows(Path::new("abc"), false));
    assert!(!set.allows(Path::new("123"), false));
    assert!(!set.allows(Path::new("a_1"), false));

    assert!(set.allows(Path::new("ab"), false));
    assert!(set.allows(Path::new("abcd"), false));
}

/// Verifies `?` at different positions.
#[test]
fn question_at_positions() {
    // At start
    let set1 = FilterSet::from_rules([FilterRule::exclude("?file.txt")]).unwrap();
    assert!(!set1.allows(Path::new("1file.txt"), false));
    assert!(set1.allows(Path::new("file.txt"), false));

    // At end
    let set2 = FilterSet::from_rules([FilterRule::exclude("file?")]).unwrap();
    assert!(!set2.allows(Path::new("file1"), false));
    assert!(set2.allows(Path::new("file"), false));

    // In middle
    let set3 = FilterSet::from_rules([FilterRule::exclude("f?le.txt")]).unwrap();
    assert!(!set3.allows(Path::new("file.txt"), false));
    assert!(!set3.allows(Path::new("fxle.txt"), false));
}

/// Verifies `?` does not match path separator.
#[test]
fn question_does_not_match_slash() {
    let set = FilterSet::from_rules([FilterRule::exclude("/a?b")]).unwrap();

    assert!(!set.allows(Path::new("aXb"), false));
    assert!(set.allows(Path::new("a/b"), false)); // Slash not matched by ?
}

// ============================================================================
// Double Star Wildcard Tests (**)
// ============================================================================

/// Verifies `**` matches across path separators.
#[test]
fn double_star_matches_slashes() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/file.txt")]).unwrap();

    assert!(!set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("a/file.txt"), false));
    assert!(!set.allows(Path::new("a/b/c/file.txt"), false));
}

/// Verifies `**` at start matches any prefix.
#[test]
fn double_star_at_start() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/test")]).unwrap();

    assert!(!set.allows(Path::new("test"), false));
    assert!(!set.allows(Path::new("src/test"), false));
    assert!(!set.allows(Path::new("a/b/c/test"), false));
}

/// Verifies `**` at end matches any suffix.
#[test]
fn double_star_at_end() {
    let set = FilterSet::from_rules([FilterRule::exclude("src/**")]).unwrap();

    assert!(!set.allows(Path::new("src/file"), false));
    assert!(!set.allows(Path::new("src/a/b/c"), false));
    assert!(set.allows(Path::new("src"), false)); // src itself not matched
}

/// Verifies `**` in middle.
#[test]
fn double_star_in_middle() {
    let set = FilterSet::from_rules([FilterRule::exclude("src/**/test.rs")]).unwrap();

    assert!(!set.allows(Path::new("src/test.rs"), false));
    assert!(!set.allows(Path::new("src/module/test.rs"), false));
    assert!(!set.allows(Path::new("src/a/b/c/test.rs"), false));

    // Does not match different base
    assert!(set.allows(Path::new("lib/test.rs"), false));
}

/// Verifies `**/` matches at root or any depth.
#[test]
fn double_star_slash() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/build/")]).unwrap();

    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("src/build"), true));
    assert!(!set.allows(Path::new("a/b/build"), true));
}

/// Verifies multiple `**` in pattern.
#[test]
fn multiple_double_stars() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/src/**/test")]).unwrap();

    assert!(!set.allows(Path::new("src/test"), false));
    assert!(!set.allows(Path::new("src/module/test"), false));
    assert!(!set.allows(Path::new("project/src/test"), false));
    assert!(!set.allows(Path::new("project/src/lib/test"), false));
}

// ============================================================================
// Character Class Tests ([])
// ============================================================================

/// Verifies basic character class.
#[test]
fn character_class_basic() {
    let set = FilterSet::from_rules([FilterRule::exclude("file[123].txt")]).unwrap();

    assert!(!set.allows(Path::new("file1.txt"), false));
    assert!(!set.allows(Path::new("file2.txt"), false));
    assert!(!set.allows(Path::new("file3.txt"), false));

    assert!(set.allows(Path::new("file4.txt"), false));
    assert!(set.allows(Path::new("filea.txt"), false));
}

/// Verifies character class with range.
#[test]
fn character_class_range() {
    let set = FilterSet::from_rules([FilterRule::exclude("file[a-z].txt")]).unwrap();

    assert!(!set.allows(Path::new("filea.txt"), false));
    assert!(!set.allows(Path::new("filem.txt"), false));
    assert!(!set.allows(Path::new("filez.txt"), false));

    assert!(set.allows(Path::new("fileA.txt"), false)); // Uppercase
    assert!(set.allows(Path::new("file1.txt"), false)); // Digit
}

/// Verifies character class with numeric range.
#[test]
fn character_class_numeric_range() {
    let set = FilterSet::from_rules([FilterRule::exclude("log[0-9].txt")]).unwrap();

    assert!(!set.allows(Path::new("log0.txt"), false));
    assert!(!set.allows(Path::new("log5.txt"), false));
    assert!(!set.allows(Path::new("log9.txt"), false));

    assert!(set.allows(Path::new("loga.txt"), false));
}

/// Verifies negated character class with !.
#[test]
fn character_class_negation_exclamation() {
    let set = FilterSet::from_rules([FilterRule::exclude("file[!0-9].txt")]).unwrap();

    // Non-digits excluded
    assert!(!set.allows(Path::new("filea.txt"), false));
    assert!(!set.allows(Path::new("fileX.txt"), false));
    assert!(!set.allows(Path::new("file_.txt"), false));

    // Digits allowed
    assert!(set.allows(Path::new("file0.txt"), false));
    assert!(set.allows(Path::new("file9.txt"), false));
}

/// Verifies negated character class with ^.
#[test]
fn character_class_negation_caret() {
    let set = FilterSet::from_rules([FilterRule::exclude("file[^a-z].txt")]).unwrap();

    // Lowercase excluded
    assert!(set.allows(Path::new("filea.txt"), false));
    assert!(set.allows(Path::new("filez.txt"), false));

    // Non-lowercase allowed
    assert!(!set.allows(Path::new("file1.txt"), false));
    assert!(!set.allows(Path::new("fileA.txt"), false));
}

/// Verifies character class with multiple ranges.
#[test]
fn character_class_multiple_ranges() {
    let set = FilterSet::from_rules([FilterRule::exclude("file[a-zA-Z0-9].txt")]).unwrap();

    // Alphanumeric excluded
    assert!(!set.allows(Path::new("filea.txt"), false));
    assert!(!set.allows(Path::new("fileZ.txt"), false));
    assert!(!set.allows(Path::new("file5.txt"), false));

    // Non-alphanumeric allowed
    assert!(set.allows(Path::new("file_.txt"), false));
    assert!(set.allows(Path::new("file-.txt"), false));
}

/// Verifies character class with literal hyphen.
#[test]
fn character_class_literal_hyphen() {
    // Hyphen at end is literal
    let set = FilterSet::from_rules([FilterRule::exclude("file[ab-].txt")]).unwrap();

    assert!(!set.allows(Path::new("filea.txt"), false));
    assert!(!set.allows(Path::new("fileb.txt"), false));
    assert!(!set.allows(Path::new("file-.txt"), false));
    assert!(set.allows(Path::new("filec.txt"), false));
}

/// Verifies multiple character classes.
#[test]
fn multiple_character_classes() {
    let set = FilterSet::from_rules([FilterRule::exclude("[a-z][0-9][A-Z]")]).unwrap();

    assert!(!set.allows(Path::new("a1A"), false));
    assert!(!set.allows(Path::new("z9Z"), false));
    assert!(!set.allows(Path::new("m5X"), false));

    assert!(set.allows(Path::new("A1a"), false)); // Wrong order
    assert!(set.allows(Path::new("aa1"), false)); // Wrong order
}

// ============================================================================
// Combined Wildcard Tests
// ============================================================================

/// Verifies `*` and `?` together.
#[test]
fn star_and_question() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.?")]).unwrap();

    assert!(!set.allows(Path::new("file.a"), false));
    assert!(!set.allows(Path::new("readme.1"), false));

    assert!(set.allows(Path::new("file.txt"), false)); // Two char extension
}

/// Verifies `*` and character class together.
#[test]
fn star_and_char_class() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.[ch]")]).unwrap();

    assert!(!set.allows(Path::new("main.c"), false));
    assert!(!set.allows(Path::new("header.h"), false));

    assert!(set.allows(Path::new("main.cpp"), false));
}

/// Verifies `**` and `*` together.
#[test]
fn double_star_and_star() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/*.o")]).unwrap();

    assert!(!set.allows(Path::new("main.o"), false));
    assert!(!set.allows(Path::new("build/main.o"), false));
    assert!(!set.allows(Path::new("build/release/main.o"), false));

    assert!(set.allows(Path::new("main.c"), false));
}

/// Verifies `**` and `?` together.
#[test]
fn double_star_and_question() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/?.txt")]).unwrap();

    assert!(!set.allows(Path::new("a.txt"), false));
    assert!(!set.allows(Path::new("dir/b.txt"), false));

    assert!(set.allows(Path::new("ab.txt"), false));
}

/// Verifies all wildcards together.
#[test]
fn all_wildcards() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/test_?_*.[ch]")]).unwrap();

    assert!(!set.allows(Path::new("test_1_foo.c"), false));
    assert!(!set.allows(Path::new("dir/test_a_bar.h"), false));

    assert!(set.allows(Path::new("test_12_foo.c"), false)); // Two chars after first _
}

// ============================================================================
// Pattern Matching at Different Depths
// ============================================================================

/// Verifies unanchored pattern matches at any depth.
#[test]
fn unanchored_matches_any_depth() {
    let set = FilterSet::from_rules([FilterRule::exclude("target")]).unwrap();

    assert!(!set.allows(Path::new("target"), false));
    assert!(!set.allows(Path::new("a/target"), false));
    assert!(!set.allows(Path::new("a/b/c/target"), false));
}

/// Verifies anchored pattern matches only at root.
#[test]
fn anchored_matches_only_root() {
    let set = FilterSet::from_rules([FilterRule::exclude("/target")]).unwrap();

    assert!(!set.allows(Path::new("target"), false));
    assert!(set.allows(Path::new("a/target"), false)); // Not at root
}

/// Verifies pattern with internal slash matches path segment.
#[test]
fn internal_slash_matches_segment() {
    let set = FilterSet::from_rules([FilterRule::exclude("build/output")]).unwrap();

    // Matches the path segment anywhere
    assert!(!set.allows(Path::new("build/output"), false));
    assert!(!set.allows(Path::new("project/build/output"), false));

    // Does not match different path
    assert!(set.allows(Path::new("src/output"), false));
}

// ============================================================================
// Directory Pattern Tests
// ============================================================================

/// Verifies directory-only pattern matches only directories.
#[test]
fn directory_only_matches_directories() {
    let set = FilterSet::from_rules([FilterRule::exclude("build/")]).unwrap();

    assert!(!set.allows(Path::new("build"), true)); // Directory
    assert!(set.allows(Path::new("build"), false)); // File
}

/// Verifies directory pattern includes descendants.
#[test]
fn directory_pattern_includes_descendants() {
    let set = FilterSet::from_rules([FilterRule::exclude("node_modules/")]).unwrap();

    assert!(!set.allows(Path::new("node_modules"), true));
    assert!(!set.allows(Path::new("node_modules/lodash"), false));
    assert!(!set.allows(Path::new("node_modules/a/b/c"), false));
}

/// Verifies nested directory patterns.
#[test]
fn nested_directory_patterns() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/node_modules/")]).unwrap();

    assert!(!set.allows(Path::new("node_modules"), true));
    assert!(!set.allows(Path::new("packages/app/node_modules"), true));
    assert!(!set.allows(Path::new("packages/app/node_modules/pkg"), false));
}

// ============================================================================
// Complex Real-World Patterns
// ============================================================================

/// Verifies gitignore-style patterns.
#[test]
fn gitignore_style_patterns() {
    let rules = [
        FilterRule::exclude("*.log"),
        FilterRule::exclude("*.tmp"),
        FilterRule::exclude("build/"),
        FilterRule::exclude(".git/"),
        FilterRule::exclude("**/node_modules/"),
        FilterRule::include("!important.log"), // Note: this is include, not negation
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("debug.log"), false));
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    assert!(!set.allows(Path::new("build/output"), false));
    assert!(!set.allows(Path::new(".git/config"), false));
    assert!(!set.allows(Path::new("packages/node_modules"), true));
}

/// Verifies Rust project patterns.
#[test]
fn rust_project_patterns() {
    let rules = [
        FilterRule::exclude("/target/"),
        FilterRule::exclude("**/*.rs.bk"),
        FilterRule::exclude("Cargo.lock").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("target/debug"), false));
    assert!(!set.allows(Path::new("src/main.rs.bk"), false));
    assert!(!set.allows(Path::new("Cargo.lock"), false));

    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("Cargo.toml"), false));
}

/// Verifies JavaScript project patterns.
#[test]
fn javascript_project_patterns() {
    // With first-match-wins, specific include must come before general exclude
    let rules = [
        FilterRule::exclude("node_modules/"),
        FilterRule::exclude("dist/"),
        FilterRule::exclude("*.min.js"),
        FilterRule::exclude("*.min.css"),
        FilterRule::include(".env.example"), // Include first
        FilterRule::exclude(".env*"),        // Then exclude pattern
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("node_modules/react"), false));
    assert!(!set.allows(Path::new("dist/bundle.js"), false));
    assert!(!set.allows(Path::new("app.min.js"), false));
    assert!(!set.allows(Path::new(".env.local"), false));

    assert!(set.allows(Path::new(".env.example"), false));
    assert!(set.allows(Path::new("src/app.js"), false));
}
