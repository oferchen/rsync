//! Tests for directory-only patterns.
//!
//! In rsync filter rules:
//! - Patterns ending with `/` only match directories
//! - Files with the same name are not matched
//! - This is useful for excluding build directories, caches, etc.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// =============================================================================
// Basic Directory-Only Tests
// =============================================================================

#[test]
fn directory_only_pattern_matches_directory() {
    let rules = [FilterRule::exclude("build/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches directory
    assert!(!set.allows(Path::new("build"), true));
}

#[test]
fn directory_only_pattern_does_not_match_file() {
    let rules = [FilterRule::exclude("build/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Does not match file with same name
    assert!(set.allows(Path::new("build"), false));
}

#[test]
fn pattern_without_trailing_slash_matches_both() {
    let rules = [FilterRule::exclude("target"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches both file and directory
    assert!(!set.allows(Path::new("target"), false));
    assert!(!set.allows(Path::new("target"), true));
}

// =============================================================================
// Wildcard Directory-Only Patterns
// =============================================================================

#[test]
fn wildcard_directory_only() {
    let rules = [FilterRule::exclude("*/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches any directory at current level
    assert!(!set.allows(Path::new("foo"), true));
    assert!(!set.allows(Path::new("bar"), true));
    // Does not match files
    assert!(set.allows(Path::new("foo"), false));
    assert!(set.allows(Path::new("bar.txt"), false));
}

#[test]
fn double_star_directory_only() {
    let rules = [FilterRule::exclude("**/cache/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches cache directory at any depth
    assert!(!set.allows(Path::new("cache"), true));
    assert!(!set.allows(Path::new("a/cache"), true));
    assert!(!set.allows(Path::new("a/b/c/cache"), true));
    // Does not match file named cache
    assert!(set.allows(Path::new("cache"), false));
    assert!(set.allows(Path::new("a/cache"), false));
}

#[test]
fn pattern_with_extension_directory_only() {
    let rules = [FilterRule::exclude("*.tmp/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches directories with .tmp extension
    assert!(!set.allows(Path::new("session.tmp"), true));
    assert!(!set.allows(Path::new("data.tmp"), true));
    // Does not match files with .tmp extension
    assert!(set.allows(Path::new("file.tmp"), false));
}

// =============================================================================
// Anchored Directory-Only Patterns
// =============================================================================

#[test]
fn anchored_directory_only() {
    let rules = [FilterRule::exclude("/dist/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Only matches at root
    assert!(!set.allows(Path::new("dist"), true));
    // Does not match nested
    assert!(set.allows(Path::new("packages/dist"), true));
    // Does not match file
    assert!(set.allows(Path::new("dist"), false));
}

#[test]
fn unanchored_directory_only() {
    let rules = [
        FilterRule::exclude("node_modules/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches at any level
    assert!(!set.allows(Path::new("node_modules"), true));
    assert!(!set.allows(Path::new("packages/foo/node_modules"), true));
    assert!(!set.allows(Path::new("a/b/c/node_modules"), true));
}

// =============================================================================
// Directory Contents Exclusion
// =============================================================================

#[test]
fn directory_contents_excluded_with_directory() {
    let rules = [FilterRule::exclude("build/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // When directory is excluded, contents are also excluded
    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("build/output.o"), false));
    assert!(!set.allows(Path::new("build/debug/binary"), false));
}

#[test]
fn nested_directory_contents() {
    let rules = [
        FilterRule::exclude("__pycache__/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Nested __pycache__ and its contents
    assert!(!set.allows(Path::new("src/__pycache__"), true));
    assert!(!set.allows(Path::new("src/__pycache__/module.pyc"), false));
    assert!(!set.allows(Path::new("src/submod/__pycache__"), true));
    assert!(!set.allows(Path::new("src/submod/__pycache__/cache.pyc"), false));
}

// =============================================================================
// Multiple Directory-Only Rules
// =============================================================================

#[test]
fn multiple_directory_only_patterns() {
    let rules = [
        FilterRule::exclude("build/"),
        FilterRule::exclude("dist/"),
        FilterRule::exclude("node_modules/"),
        FilterRule::exclude(".git/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All directories excluded
    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("dist"), true));
    assert!(!set.allows(Path::new("node_modules"), true));
    assert!(!set.allows(Path::new(".git"), true));

    // Files with same names included
    assert!(set.allows(Path::new("build"), false));
    assert!(set.allows(Path::new("dist"), false));
}

#[test]
fn directory_only_with_other_patterns() {
    let rules = [
        FilterRule::exclude("target/"), // Directory only
        FilterRule::exclude("*.bak"),   // Files
        FilterRule::include("src/"),    // Include directory
        FilterRule::exclude("**"),      // Default exclude
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory rule
    assert!(!set.allows(Path::new("target"), true));
    assert!(!set.allows(Path::new("target"), false)); // Falls through to default

    // File pattern
    assert!(!set.allows(Path::new("file.bak"), false));

    // Include directory
    assert!(set.allows(Path::new("src"), true));
    assert!(set.allows(Path::new("src/main.rs"), false));
}

// =============================================================================
// Include Directory-Only Patterns
// =============================================================================

#[test]
fn include_directory_only() {
    let rules = [FilterRule::include("important/"), FilterRule::exclude("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory included
    assert!(set.allows(Path::new("important"), true));
    assert!(set.allows(Path::new("important/file.txt"), false));
    // File with same name excluded
    assert!(!set.allows(Path::new("important"), false));
}

#[test]
fn include_specific_directories_exclude_others() {
    let rules = [
        FilterRule::include("src/"),
        FilterRule::include("tests/"),
        FilterRule::include("docs/"),
        FilterRule::exclude("*/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Specific directories included
    assert!(set.allows(Path::new("src"), true));
    assert!(set.allows(Path::new("tests"), true));
    assert!(set.allows(Path::new("docs"), true));

    // Other directories excluded
    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("tmp"), true));

    // Files always included (last rule)
    assert!(set.allows(Path::new("README.md"), false));
}

// =============================================================================
// Protect/Risk Directory-Only
// =============================================================================

#[test]
fn protect_directory_only() {
    let rule = FilterRule::protect("/data/");
    assert_eq!(rule.pattern(), "/data/");
    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn risk_directory_only() {
    let rule = FilterRule::risk("/tmp/");
    assert_eq!(rule.pattern(), "/tmp/");
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn empty_directory_name_with_slash() {
    let rule = FilterRule::exclude("/");
    assert_eq!(rule.pattern(), "/");
}

#[test]
fn multiple_trailing_slashes() {
    // Multiple trailing slashes - behavior may vary
    let rule = FilterRule::exclude("dir//");
    assert_eq!(rule.pattern(), "dir//");
}

#[test]
fn directory_only_with_special_characters() {
    let rules = [
        FilterRule::exclude(".hidden/"),
        FilterRule::exclude("__pycache__/"),
        FilterRule::exclude("node_modules/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new(".hidden"), true));
    assert!(!set.allows(Path::new("__pycache__"), true));
    assert!(!set.allows(Path::new("node_modules"), true));
}

#[test]
fn directory_only_with_dots() {
    let rules = [FilterRule::exclude("..tmp/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("..tmp"), true));
    assert!(set.allows(Path::new("..tmp"), false));
}

#[test]
fn directory_only_unicode() {
    let rules = [
        FilterRule::exclude("кэш/"), // Russian "cache"
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("кэш"), true));
    assert!(set.allows(Path::new("кэш"), false));
}

// =============================================================================
// Real-World Scenarios
// =============================================================================

#[test]
fn python_project_directories() {
    let rules = [
        // Build artifacts
        FilterRule::exclude("__pycache__/"),
        FilterRule::exclude("*.egg-info/"),
        FilterRule::exclude(".eggs/"),
        FilterRule::exclude("dist/"),
        FilterRule::exclude("build/"),
        // Virtual environments
        FilterRule::exclude("venv/"),
        FilterRule::exclude(".venv/"),
        FilterRule::exclude("env/"),
        // IDE
        FilterRule::exclude(".idea/"),
        FilterRule::exclude(".vscode/"),
        // Testing
        FilterRule::exclude(".pytest_cache/"),
        FilterRule::exclude(".coverage/"),
        FilterRule::exclude("htmlcov/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All directory patterns work
    assert!(!set.allows(Path::new("__pycache__"), true));
    assert!(!set.allows(Path::new("src/__pycache__"), true));
    assert!(!set.allows(Path::new("venv"), true));
    assert!(!set.allows(Path::new(".pytest_cache"), true));

    // Files with same names are included
    assert!(set.allows(Path::new("venv"), false));

    // Source files included
    assert!(set.allows(Path::new("src/main.py"), false));
}

#[test]
fn rust_project_directories() {
    let rules = [
        FilterRule::exclude("/target/"),
        FilterRule::exclude(".cargo/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root target excluded
    assert!(!set.allows(Path::new("target"), true));
    assert!(!set.allows(Path::new("target/debug/binary"), false));
    // Nested target (in workspace) included because of anchored pattern
    assert!(set.allows(Path::new("crates/foo/target"), true));

    // .cargo excluded anywhere
    assert!(!set.allows(Path::new(".cargo"), true));
    assert!(!set.allows(Path::new("home/.cargo"), true));
}

#[test]
fn javascript_project_directories() {
    let rules = [
        FilterRule::exclude("node_modules/"),
        FilterRule::exclude("bower_components/"),
        FilterRule::exclude(".npm/"),
        FilterRule::exclude("/dist/"),
        FilterRule::exclude("/build/"),
        FilterRule::exclude(".cache/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // node_modules anywhere
    assert!(!set.allows(Path::new("node_modules"), true));
    assert!(!set.allows(Path::new("packages/foo/node_modules"), true));

    // dist/build only at root
    assert!(!set.allows(Path::new("dist"), true));
    assert!(set.allows(Path::new("packages/foo/dist"), true));

    // .cache anywhere
    assert!(!set.allows(Path::new(".cache"), true));
}

#[test]
fn general_build_directories() {
    let rules = [
        // Common build directory names
        FilterRule::exclude("build/"),
        FilterRule::exclude("dist/"),
        FilterRule::exclude("out/"),
        FilterRule::exclude("output/"),
        FilterRule::exclude("bin/"),
        FilterRule::exclude("obj/"),
        FilterRule::exclude("lib/"),
        // Version control
        FilterRule::exclude(".git/"),
        FilterRule::exclude(".svn/"),
        FilterRule::exclude(".hg/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All directories excluded
    for dir in &[
        "build", "dist", "out", "output", "bin", "obj", "lib", ".git", ".svn", ".hg",
    ] {
        assert!(
            !set.allows(Path::new(dir), true),
            "{dir} should be excluded as directory"
        );
    }

    // As files, they should be included
    for file in &["build", "dist", "out", "output", "bin", "obj", "lib"] {
        assert!(
            set.allows(Path::new(file), false),
            "{file} should be included as file"
        );
    }
}

// =============================================================================
// Interaction with Other Features
// =============================================================================

#[test]
fn directory_only_with_negation() {
    // Negated directory-only pattern excludes files but not directories
    let rules = [
        FilterRule::exclude("cache/").with_negate(true),
        FilterRule::exclude("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Negated: excludes things that DON'T match cache/
    // So cache directory itself is included (matches, negated = no exclude)
    // Everything else is excluded
    assert!(!set.allows(Path::new("cache"), true)); // Falls through to **
    assert!(!set.allows(Path::new("other"), true));
}

#[test]
fn directory_only_with_perishable() {
    let rule = FilterRule::exclude("tmp/").with_perishable(true);
    assert!(rule.is_perishable());
    assert_eq!(rule.pattern(), "tmp/");
}

#[test]
fn directory_only_sender_side() {
    let rules = [FilterRule::hide("secret/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Hidden directory - `allows` uses sender context by default for transfer
    assert!(!set.allows(Path::new("secret"), true));
}
