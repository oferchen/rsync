//! Comprehensive tests for --include pattern matching.
//!
//! These tests verify that include patterns work correctly across various
//! pattern types and interact properly with exclude rules. rsync uses
//! first-match-wins semantics, meaning include rules must come before
//! exclude rules to create exceptions.
//!
//! Reference: rsync 3.4.1 exclude.c for pattern matching semantics.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// 1. Simple Filename Patterns (*.txt, *.rs, etc.)
// ============================================================================

/// Verifies simple extension-based include pattern.
#[test]
fn include_simple_extension_pattern() {
    let rules = [FilterRule::include("*.txt"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // .txt files should be included
    assert!(set.allows(Path::new("readme.txt"), false));
    assert!(set.allows(Path::new("notes.txt"), false));
    assert!(set.allows(Path::new("a.txt"), false));

    // Other extensions should be excluded
    assert!(!set.allows(Path::new("readme.md"), false));
    assert!(!set.allows(Path::new("main.rs"), false));
    assert!(!set.allows(Path::new("file.log"), false));
}

/// Verifies include pattern with prefix wildcard.
#[test]
fn include_prefix_wildcard() {
    let rules = [FilterRule::include("test_*"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("test_main.rs"), false));
    assert!(set.allows(Path::new("test_utils"), false));
    assert!(set.allows(Path::new("test_"), false));

    assert!(!set.allows(Path::new("main_test.rs"), false));
    assert!(!set.allows(Path::new("mytest_file"), false));
}

/// Verifies include pattern with suffix wildcard.
#[test]
fn include_suffix_wildcard() {
    let rules = [FilterRule::include("*_test"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("unit_test"), false));
    assert!(set.allows(Path::new("integration_test"), false));

    assert!(!set.allows(Path::new("test_unit"), false));
}

/// Verifies include pattern with wildcard in the middle.
#[test]
fn include_middle_wildcard() {
    let rules = [FilterRule::include("file_*_v2"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("file_data_v2"), false));
    assert!(set.allows(Path::new("file_config_v2"), false));
    assert!(set.allows(Path::new("file__v2"), false));

    assert!(!set.allows(Path::new("file_data_v1"), false));
    assert!(!set.allows(Path::new("data_file_v2"), false));
}

/// Verifies include pattern with question mark single-char wildcard.
#[test]
fn include_question_mark_wildcard() {
    let rules = [FilterRule::include("log?.txt"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("log1.txt"), false));
    assert!(set.allows(Path::new("logA.txt"), false));
    assert!(set.allows(Path::new("log_.txt"), false));

    // ? matches exactly one character
    assert!(!set.allows(Path::new("log.txt"), false));
    assert!(!set.allows(Path::new("log12.txt"), false));
}

/// Verifies include pattern with character class.
#[test]
fn include_character_class() {
    let rules = [
        FilterRule::include("data[0-9].csv"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("data0.csv"), false));
    assert!(set.allows(Path::new("data5.csv"), false));
    assert!(set.allows(Path::new("data9.csv"), false));

    assert!(!set.allows(Path::new("dataa.csv"), false));
    assert!(!set.allows(Path::new("data10.csv"), false));
}

/// Verifies include pattern with negated character class.
#[test]
fn include_negated_character_class() {
    let rules = [
        FilterRule::include("file[!0-9].txt"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Non-digits should be included
    assert!(set.allows(Path::new("filea.txt"), false));
    assert!(set.allows(Path::new("fileX.txt"), false));

    // Digits should be excluded
    assert!(!set.allows(Path::new("file0.txt"), false));
    assert!(!set.allows(Path::new("file5.txt"), false));
}

/// Verifies exact filename include pattern.
#[test]
fn include_exact_filename() {
    let rules = [
        FilterRule::include("Makefile"),
        FilterRule::include("Cargo.toml"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("Makefile"), false));
    assert!(set.allows(Path::new("Cargo.toml"), false));

    assert!(!set.allows(Path::new("makefile"), false)); // Case sensitive
    assert!(!set.allows(Path::new("cargo.toml"), false));
    assert!(!set.allows(Path::new("other.txt"), false));
}

/// Verifies multiple extension patterns.
#[test]
fn include_multiple_extensions() {
    let rules = [
        FilterRule::include("*.rs"),
        FilterRule::include("*.toml"),
        FilterRule::include("*.md"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("Cargo.toml"), false));
    assert!(set.allows(Path::new("README.md"), false));

    assert!(!set.allows(Path::new("main.py"), false));
    assert!(!set.allows(Path::new("config.json"), false));
}

// ============================================================================
// 2. Directory Patterns (dir/)
// ============================================================================

/// Verifies basic directory include pattern.
#[test]
fn include_directory_pattern() {
    let rules = [FilterRule::include("src/"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory should be included
    assert!(set.allows(Path::new("src"), true));

    // Contents of directory should be included
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("src/lib.rs"), false));
    assert!(set.allows(Path::new("src/utils/mod.rs"), false));

    // File with same name should be excluded (directory pattern)
    assert!(!set.allows(Path::new("src"), false));
}

/// Verifies nested directory include pattern.
#[test]
fn include_nested_directory_pattern() {
    let rules = [
        FilterRule::include("packages/core/"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // The nested directory path
    assert!(set.allows(Path::new("packages/core"), true));
    assert!(set.allows(Path::new("packages/core/src"), true));
    assert!(set.allows(Path::new("packages/core/src/index.ts"), false));

    // Other packages excluded
    assert!(!set.allows(Path::new("packages/utils"), true));
}

/// Verifies wildcard in directory pattern.
#[test]
fn include_wildcard_directory_pattern() {
    let rules = [FilterRule::include("test*/"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("tests"), true));
    assert!(set.allows(Path::new("testing"), true));
    assert!(set.allows(Path::new("test_fixtures"), true));
    assert!(set.allows(Path::new("tests/unit.rs"), false));

    assert!(!set.allows(Path::new("src"), true));
}

/// Verifies multiple directory patterns.
#[test]
fn include_multiple_directories() {
    let rules = [
        FilterRule::include("src/"),
        FilterRule::include("tests/"),
        FilterRule::include("docs/"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("src"), true));
    assert!(set.allows(Path::new("tests"), true));
    assert!(set.allows(Path::new("docs"), true));
    assert!(set.allows(Path::new("src/main.rs"), false));

    assert!(!set.allows(Path::new("target"), true));
    assert!(!set.allows(Path::new("node_modules"), true));
}

/// Verifies directory pattern at any depth (unanchored).
#[test]
fn include_directory_any_depth() {
    let rules = [FilterRule::include("fixtures/"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Should match at any depth
    assert!(set.allows(Path::new("fixtures"), true));
    assert!(set.allows(Path::new("tests/fixtures"), true));
    assert!(set.allows(Path::new("packages/app/tests/fixtures"), true));
    assert!(set.allows(Path::new("fixtures/data.json"), false));
}

// ============================================================================
// 3. Anchored Patterns (/root/file)
// ============================================================================

/// Verifies anchored include pattern at root.
#[test]
fn include_anchored_at_root() {
    let rules = [FilterRule::include("/Cargo.toml"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Should match only at root
    assert!(set.allows(Path::new("Cargo.toml"), false));

    // Should not match in subdirectories
    assert!(!set.allows(Path::new("crates/mylib/Cargo.toml"), false));
    assert!(!set.allows(Path::new("packages/Cargo.toml"), false));
}

/// Verifies anchored include pattern with directory.
#[test]
fn include_anchored_directory() {
    let rules = [FilterRule::include("/src/"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root src directory
    assert!(set.allows(Path::new("src"), true));
    assert!(set.allows(Path::new("src/main.rs"), false));

    // Nested src directories should NOT match
    assert!(!set.allows(Path::new("crates/lib/src"), true));
}

/// Verifies anchored path pattern with multiple components.
#[test]
fn include_anchored_multi_component() {
    let rules = [FilterRule::include("/src/bin/"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("src/bin"), true));
    assert!(set.allows(Path::new("src/bin/main.rs"), false));

    assert!(!set.allows(Path::new("other/src/bin"), true));
}

/// Verifies anchored pattern with wildcard.
#[test]
fn include_anchored_with_wildcard() {
    let rules = [
        FilterRule::include("/config/*.json"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("config/app.json"), false));
    assert!(set.allows(Path::new("config/db.json"), false));

    // Nested config directories should not match
    assert!(!set.allows(Path::new("packages/config/app.json"), false));

    // Non-json files should not match
    assert!(!set.allows(Path::new("config/app.yaml"), false));
}

/// Verifies unanchored pattern with slash is matched at any depth.
#[test]
fn include_unanchored_with_slash() {
    // Pattern with internal slash is anchored (rsync semantics)
    let rules = [FilterRule::include("src/main.rs"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Should match at root
    assert!(set.allows(Path::new("src/main.rs"), false));

    // Pattern with internal slash is anchored, so only matches at root
    // To match at any depth, use **/src/main.rs
    assert!(!set.allows(Path::new("project/src/main.rs"), false));
}

/// Verifies anchor_to_root method.
#[test]
fn include_anchor_to_root_method() {
    let rules = [
        FilterRule::include("config.json").anchor_to_root(),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("config.json"), false));
    assert!(!set.allows(Path::new("app/config.json"), false));
}

// ============================================================================
// 4. Double-Star Patterns (**/deep/file)
// ============================================================================

/// Verifies leading double-star pattern.
#[test]
fn include_leading_double_star() {
    let rules = [FilterRule::include("**/test.rs"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Should match at any depth
    assert!(set.allows(Path::new("test.rs"), false));
    assert!(set.allows(Path::new("src/test.rs"), false));
    assert!(set.allows(Path::new("crates/mylib/src/test.rs"), false));
    assert!(set.allows(Path::new("a/b/c/d/e/test.rs"), false));

    // Different filename should not match
    assert!(!set.allows(Path::new("main.rs"), false));
}

/// Verifies trailing double-star pattern.
#[test]
fn include_trailing_double_star() {
    let rules = [FilterRule::include("docs/**"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // All contents under docs
    assert!(set.allows(Path::new("docs/readme.md"), false));
    assert!(set.allows(Path::new("docs/api/index.html"), false));
    assert!(set.allows(Path::new("docs/guides/getting-started.md"), false));

    // The docs directory itself is not matched by /**
    assert!(!set.allows(Path::new("docs"), true));

    // Other directories not matched
    assert!(!set.allows(Path::new("src/file.rs"), false));
}

/// Verifies double-star in middle of pattern.
#[test]
fn include_middle_double_star() {
    let rules = [
        FilterRule::include("src/**/mod.rs"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("src/mod.rs"), false));
    assert!(set.allows(Path::new("src/utils/mod.rs"), false));
    assert!(set.allows(Path::new("src/a/b/c/mod.rs"), false));

    // Different base path
    assert!(!set.allows(Path::new("lib/mod.rs"), false));

    // Different filename
    assert!(!set.allows(Path::new("src/lib.rs"), false));
}

/// Verifies double-star with directory pattern.
#[test]
fn include_double_star_directory() {
    let rules = [
        FilterRule::include("**/node_modules/"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("node_modules"), true));
    assert!(set.allows(Path::new("packages/app/node_modules"), true));
    assert!(set.allows(Path::new("node_modules/lodash"), false));

    // File named node_modules should not match
    assert!(!set.allows(Path::new("node_modules"), false));
}

/// Verifies multiple double-stars in pattern.
#[test]
fn include_multiple_double_stars() {
    let rules = [
        FilterRule::include("**/tests/**/fixtures/**"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("tests/fixtures/data.json"), false));
    assert!(set.allows(Path::new("tests/unit/fixtures/mock.js"), false));
    assert!(set.allows(
        Path::new("packages/app/tests/integration/fixtures/setup.ts"),
        false
    ));

    assert!(!set.allows(Path::new("tests/unit/test.js"), false));
}

/// Verifies double-star with extension pattern.
#[test]
fn include_double_star_extension() {
    let rules = [FilterRule::include("**/*.rs"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("src/lib.rs"), false));
    assert!(set.allows(Path::new("crates/mylib/src/utils.rs"), false));

    assert!(!set.allows(Path::new("Cargo.toml"), false));
    assert!(!set.allows(Path::new("src/Cargo.toml"), false));
}

// ============================================================================
// 5. Interaction with --exclude (include overrides exclude)
// ============================================================================

/// Verifies include before exclude creates exception (first-match-wins).
#[test]
fn include_overrides_exclude_first_match() {
    let rules = [
        FilterRule::include("important.log"),
        FilterRule::exclude("*.log"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Specific file is included (first rule matches)
    assert!(set.allows(Path::new("important.log"), false));

    // Other .log files are excluded (second rule matches)
    assert!(!set.allows(Path::new("debug.log"), false));
    assert!(!set.allows(Path::new("error.log"), false));
}

/// Verifies include directory exception within excluded parent.
#[test]
fn include_directory_exception() {
    let rules = [
        FilterRule::include("target/doc/"),
        FilterRule::exclude("target/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Doc directory is included (first rule)
    assert!(set.allows(Path::new("target/doc"), true));
    assert!(set.allows(Path::new("target/doc/index.html"), false));

    // Other target contents are excluded (second rule)
    assert!(!set.allows(Path::new("target/debug"), true));
    assert!(!set.allows(Path::new("target/release/binary"), false));
}

/// Verifies include pattern within excluded path using double-star.
#[test]
fn include_within_excluded_path() {
    let rules = [
        FilterRule::include("**/README.md"),
        FilterRule::exclude("vendor/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // README.md files everywhere are included (first rule)
    assert!(set.allows(Path::new("vendor/lib/README.md"), false));
    assert!(set.allows(Path::new("README.md"), false));

    // Other vendor files are excluded (second rule)
    assert!(!set.allows(Path::new("vendor/lib/index.js"), false));
}

/// Verifies include extension exception within excluded pattern.
#[test]
fn include_extension_exception() {
    let rules = [FilterRule::include("*.rs"), FilterRule::exclude("test_*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // .rs files are included (first rule)
    assert!(set.allows(Path::new("test_main.rs"), false));
    assert!(set.allows(Path::new("main.rs"), false));

    // Non-.rs test files are excluded (second rule)
    assert!(!set.allows(Path::new("test_data.json"), false));
}

/// Verifies multiple includes creating exceptions for single exclude.
#[test]
fn multiple_includes_single_exclude() {
    let rules = [
        FilterRule::include("*.rs"),
        FilterRule::include("*.toml"),
        FilterRule::include("*.md"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("Cargo.toml"), false));
    assert!(set.allows(Path::new("README.md"), false));

    assert!(!set.allows(Path::new("script.py"), false));
    assert!(!set.allows(Path::new("config.json"), false));
}

/// Verifies exclude overriding previous include when ordered correctly.
#[test]
fn exclude_after_include_takes_precedence() {
    // With first-match-wins, the FIRST matching rule wins
    // So if we want to exclude test_*.rs but include *.rs,
    // the exclude must come first
    let rules = [
        FilterRule::exclude("test_*.rs"),
        FilterRule::include("*.rs"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // test_*.rs excluded (first rule)
    assert!(!set.allows(Path::new("test_main.rs"), false));

    // Other .rs files included (second rule)
    assert!(set.allows(Path::new("main.rs"), false));
}

/// Verifies complex include/exclude interaction.
#[test]
fn complex_include_exclude_interaction() {
    let rules = [
        FilterRule::include("src/**/fixtures/**"), // Include fixtures in src
        FilterRule::exclude("src/**/test_*.rs"),   // Exclude test files
        FilterRule::include("src/**/*.rs"),        // Include .rs files in src
        FilterRule::exclude("*"),                  // Exclude everything else
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Fixtures are included (first rule)
    assert!(set.allows(Path::new("src/tests/fixtures/data.json"), false));

    // Test files are excluded (second rule)
    assert!(!set.allows(Path::new("src/tests/test_main.rs"), false));

    // Regular .rs files are included (third rule)
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("src/lib/utils.rs"), false));

    // Everything else excluded (fourth rule)
    assert!(!set.allows(Path::new("Cargo.toml"), false));
}

// ============================================================================
// 6. Order-Dependent Behavior
// ============================================================================

/// Verifies that rule order matters - first match wins.
#[test]
fn order_first_match_wins() {
    // Order 1: Include first
    let rules1 = [
        FilterRule::include("*.txt"),
        FilterRule::exclude("readme.txt"),
    ];
    let set1 = FilterSet::from_rules(rules1).unwrap();

    // *.txt matches first, so readme.txt is included
    assert!(set1.allows(Path::new("readme.txt"), false));

    // Order 2: Exclude first
    let rules2 = [
        FilterRule::exclude("readme.txt"),
        FilterRule::include("*.txt"),
    ];
    let set2 = FilterSet::from_rules(rules2).unwrap();

    // readme.txt is excluded (first rule matches)
    assert!(!set2.allows(Path::new("readme.txt"), false));

    // Other .txt files included (second rule)
    assert!(set2.allows(Path::new("notes.txt"), false));
}

/// Verifies ordering with multiple specific patterns.
#[test]
fn order_multiple_specific_patterns() {
    let rules = [
        FilterRule::include("a.txt"),
        FilterRule::exclude("b.txt"),
        FilterRule::include("c.txt"),
        FilterRule::exclude("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("a.txt"), false)); // First rule
    assert!(!set.allows(Path::new("b.txt"), false)); // Second rule
    assert!(set.allows(Path::new("c.txt"), false)); // Third rule
    assert!(!set.allows(Path::new("d.txt"), false)); // Fourth rule
}

/// Verifies ordering with nested patterns.
#[test]
fn order_nested_patterns() {
    let rules = [
        FilterRule::include("src/special/**"),
        FilterRule::exclude("src/**/*.bak"),
        FilterRule::include("src/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Special directory included (first rule)
    assert!(set.allows(Path::new("src/special/file.bak"), false));

    // .bak files in other src locations excluded (second rule)
    assert!(!set.allows(Path::new("src/utils/temp.bak"), false));

    // Other files included (third rule)
    assert!(set.allows(Path::new("src/main.rs"), false));
}

/// Verifies that the default is include when no rule matches.
#[test]
fn order_default_include_no_match() {
    let rules = [FilterRule::exclude("*.bak"), FilterRule::exclude("*.tmp")];
    let set = FilterSet::from_rules(rules).unwrap();

    // No rule matches, so default is include
    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("data.json"), false));
}

/// Verifies that duplicate rules don't change behavior.
#[test]
fn order_duplicate_rules() {
    let rules = [
        FilterRule::include("*.rs"),
        FilterRule::include("*.rs"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // First include matches, duplicates are harmless
    assert!(set.allows(Path::new("main.rs"), false));
}

/// Verifies clear rule resets order.
#[test]
fn order_clear_resets() {
    let rules = [
        FilterRule::exclude("*.txt"),
        FilterRule::clear(),
        FilterRule::include("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // After clear, only the include rule is active
    assert!(set.allows(Path::new("readme.txt"), false));
}

/// Verifies order with anchored and unanchored patterns.
#[test]
fn order_anchored_unanchored() {
    let rules = [
        FilterRule::include("/config.json"), // Anchored at root
        FilterRule::exclude("config.json"),  // Matches anywhere
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root config.json is included (anchored rule first)
    assert!(set.allows(Path::new("config.json"), false));

    // Nested config.json is excluded (unanchored rule)
    assert!(!set.allows(Path::new("app/config.json"), false));
}

// ============================================================================
// 7. Multiple --include Flags
// ============================================================================

/// Verifies multiple include patterns for different extensions.
#[test]
fn multiple_includes_extensions() {
    let rules = [
        FilterRule::include("*.rs"),
        FilterRule::include("*.toml"),
        FilterRule::include("*.lock"),
        FilterRule::include("*.md"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("Cargo.toml"), false));
    assert!(set.allows(Path::new("Cargo.lock"), false));
    assert!(set.allows(Path::new("README.md"), false));

    assert!(!set.allows(Path::new("script.py"), false));
    assert!(!set.allows(Path::new("index.js"), false));
}

/// Verifies multiple include patterns for different directories.
#[test]
fn multiple_includes_directories() {
    let rules = [
        FilterRule::include("src/"),
        FilterRule::include("tests/"),
        FilterRule::include("benches/"),
        FilterRule::include("examples/"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("src"), true));
    assert!(set.allows(Path::new("tests"), true));
    assert!(set.allows(Path::new("benches"), true));
    assert!(set.allows(Path::new("examples"), true));
    assert!(set.allows(Path::new("src/main.rs"), false));

    assert!(!set.allows(Path::new("target"), true));
    assert!(!set.allows(Path::new("node_modules"), true));
}

/// Verifies multiple include patterns with overlapping scope.
#[test]
fn multiple_includes_overlapping() {
    let rules = [
        FilterRule::include("**/*.rs"),
        FilterRule::include("src/**"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Both patterns include .rs files in src
    assert!(set.allows(Path::new("src/main.rs"), false));

    // First pattern includes .rs anywhere
    assert!(set.allows(Path::new("tests/test.rs"), false));

    // Second pattern includes non-.rs in src
    assert!(set.allows(Path::new("src/config.json"), false));

    // Neither matches
    assert!(!set.allows(Path::new("tests/config.json"), false));
}

/// Verifies multiple include patterns with specific and general.
#[test]
fn multiple_includes_specific_general() {
    let rules = [
        FilterRule::include("Cargo.toml"),
        FilterRule::include("Cargo.lock"),
        FilterRule::include("*.rs"),
        FilterRule::include("*.md"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Specific files
    assert!(set.allows(Path::new("Cargo.toml"), false));
    assert!(set.allows(Path::new("Cargo.lock"), false));

    // General patterns
    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("README.md"), false));

    assert!(!set.allows(Path::new("package.json"), false));
}

/// Verifies order among multiple includes doesn't affect outcome.
#[test]
fn multiple_includes_order_irrelevant() {
    // Order 1
    let rules1 = [
        FilterRule::include("*.rs"),
        FilterRule::include("*.md"),
        FilterRule::exclude("*"),
    ];
    let set1 = FilterSet::from_rules(rules1).unwrap();

    // Order 2
    let rules2 = [
        FilterRule::include("*.md"),
        FilterRule::include("*.rs"),
        FilterRule::exclude("*"),
    ];
    let set2 = FilterSet::from_rules(rules2).unwrap();

    // Both should behave the same for these files
    assert!(set1.allows(Path::new("main.rs"), false));
    assert!(set1.allows(Path::new("README.md"), false));
    assert!(set2.allows(Path::new("main.rs"), false));
    assert!(set2.allows(Path::new("README.md"), false));
}

/// Verifies many include patterns can be combined.
#[test]
fn multiple_includes_many() {
    let includes: Vec<_> = (0..100)
        .map(|i| FilterRule::include(format!("file{i}.txt")))
        .collect();
    let mut rules = includes;
    rules.push(FilterRule::exclude("*"));

    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("file50.txt"), false));
    assert!(set.allows(Path::new("file99.txt"), false));
    assert!(!set.allows(Path::new("file100.txt"), false));
    assert!(!set.allows(Path::new("other.txt"), false));
}

/// Verifies multiple includes with depth patterns.
#[test]
fn multiple_includes_depth_patterns() {
    let rules = [
        FilterRule::include("/root.txt"),
        FilterRule::include("**/nested.txt"),
        FilterRule::include("deep/**/file.txt"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root only
    assert!(set.allows(Path::new("root.txt"), false));
    assert!(!set.allows(Path::new("dir/root.txt"), false));

    // Any depth
    assert!(set.allows(Path::new("nested.txt"), false));
    assert!(set.allows(Path::new("a/b/nested.txt"), false));

    // Specific structure
    assert!(set.allows(Path::new("deep/file.txt"), false));
    assert!(set.allows(Path::new("deep/a/b/file.txt"), false));
    assert!(!set.allows(Path::new("other/file.txt"), false));
}

// ============================================================================
// Additional Edge Cases
// ============================================================================

/// Verifies include with hidden files (dot prefix).
#[test]
fn include_hidden_files() {
    let rules = [FilterRule::include(".*"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new(".gitignore"), false));
    assert!(set.allows(Path::new(".env"), false));
    assert!(set.allows(Path::new(".hidden"), true));

    assert!(!set.allows(Path::new("visible"), false));
}

/// Verifies include specific hidden files.
#[test]
fn include_specific_hidden_files() {
    let rules = [
        FilterRule::include(".gitignore"),
        FilterRule::include(".env.example"),
        FilterRule::exclude(".*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new(".gitignore"), false));
    assert!(set.allows(Path::new(".env.example"), false));
    assert!(!set.allows(Path::new(".env"), false));
    assert!(!set.allows(Path::new(".hidden"), false));
}

/// Verifies include with complex extension.
#[test]
fn include_complex_extension() {
    let rules = [
        FilterRule::include("*.tar.gz"),
        FilterRule::include("*.test.js"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("archive.tar.gz"), false));
    assert!(set.allows(Path::new("app.test.js"), false));

    assert!(!set.allows(Path::new("archive.tar"), false));
    assert!(!set.allows(Path::new("app.js"), false));
}

/// Verifies include with paths containing spaces.
#[test]
fn include_paths_with_spaces() {
    let rules = [
        FilterRule::include("my file.txt"),
        FilterRule::include("folder with spaces/"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("my file.txt"), false));
    assert!(set.allows(Path::new("folder with spaces"), true));
    assert!(set.allows(Path::new("folder with spaces/data.txt"), false));
}

/// Verifies include pattern case sensitivity.
#[test]
fn include_case_sensitivity() {
    let rules = [FilterRule::include("README.md"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Exact case matches
    assert!(set.allows(Path::new("README.md"), false));

    // Different case doesn't match (rsync is case-sensitive by default)
    assert!(!set.allows(Path::new("readme.md"), false));
    assert!(!set.allows(Path::new("Readme.md"), false));
    assert!(!set.allows(Path::new("README.MD"), false));
}

/// Verifies include empty pattern behavior.
#[test]
fn include_empty_pattern() {
    // Empty pattern in include - should compile without error
    let set = FilterSet::from_rules([FilterRule::include("")]).unwrap();
    assert!(!set.is_empty());
}

/// Verifies include with show (sender-only include).
#[test]
fn include_show_sender_only() {
    let rules = [FilterRule::show("*.log"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Show is sender-only include, so allows() checks sender side
    assert!(set.allows(Path::new("app.log"), false));

    // But receiver side (allows_deletion) doesn't see the show rule
    // Note: allows_deletion checks receiver-side rules
    // Since show doesn't apply to receiver, only the exclude applies
    assert!(!set.allows_deletion(Path::new("app.log"), false));
}

/// Verifies include pattern with all modifier flags.
#[test]
fn include_with_modifiers() {
    let rule = FilterRule::include("*.rs")
        .with_perishable(false)
        .with_sender(true)
        .with_receiver(true);

    let set = FilterSet::from_rules([rule, FilterRule::exclude("*")]).unwrap();

    assert!(set.allows(Path::new("main.rs"), false));
}
