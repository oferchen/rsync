//! Tests for anchored vs unanchored patterns.
//!
//! In rsync filter rules:
//! - Anchored patterns start with `/` and match from the root of the transfer
//! - Unanchored patterns can match at any level in the directory tree
//! - Patterns containing `/` (other than leading) are anchored by default

use filters::{FilterRule, FilterSet};
use std::path::Path;

// =============================================================================
// Basic Anchoring Tests
// =============================================================================

#[test]
fn anchored_pattern_with_leading_slash() {
    let rules = [FilterRule::exclude("/root.txt"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches at root
    assert!(!set.allows(Path::new("root.txt"), false));
    // Does not match in subdirectory
    assert!(set.allows(Path::new("subdir/root.txt"), false));
    assert!(set.allows(Path::new("a/b/root.txt"), false));
}

#[test]
fn unanchored_pattern_without_slash() {
    let rules = [FilterRule::exclude("test.txt"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches at any level
    assert!(!set.allows(Path::new("test.txt"), false));
    assert!(!set.allows(Path::new("subdir/test.txt"), false));
    assert!(!set.allows(Path::new("a/b/c/test.txt"), false));
}

#[test]
fn wildcard_anchored_at_root() {
    let rules = [FilterRule::exclude("/*.txt"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches .txt at root only
    assert!(!set.allows(Path::new("readme.txt"), false));
    assert!(!set.allows(Path::new("notes.txt"), false));
    // Does not match in subdirectories
    assert!(set.allows(Path::new("docs/readme.txt"), false));
    assert!(set.allows(Path::new("a/b/notes.txt"), false));
}

#[test]
fn wildcard_unanchored() {
    let rules = [FilterRule::exclude("*.txt"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches at any level
    assert!(!set.allows(Path::new("readme.txt"), false));
    assert!(!set.allows(Path::new("docs/readme.txt"), false));
    assert!(!set.allows(Path::new("a/b/c/notes.txt"), false));
}

// =============================================================================
// Patterns with Internal Slash (Implicitly Anchored)
// =============================================================================

#[test]
fn pattern_with_internal_slash_is_anchored() {
    let rules = [
        FilterRule::exclude("src/test.txt"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Pattern with / is anchored to root
    assert!(!set.allows(Path::new("src/test.txt"), false));
    // Does not match in nested paths
    assert!(set.allows(Path::new("project/src/test.txt"), false));
}

#[test]
fn pattern_with_double_star_and_internal_slash() {
    let rules = [
        FilterRule::exclude("**/test.txt"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // ** allows matching at any depth
    assert!(!set.allows(Path::new("test.txt"), false));
    assert!(!set.allows(Path::new("a/test.txt"), false));
    assert!(!set.allows(Path::new("a/b/c/test.txt"), false));
}

#[test]
fn anchored_directory_pattern() {
    let rules = [FilterRule::exclude("/build/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches build directory at root only
    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("build/output"), false));
    // Does not match nested build directories
    assert!(set.allows(Path::new("project/build"), true));
}

// =============================================================================
// anchor_to_root() Method Tests
// =============================================================================

#[test]
fn anchor_to_root_adds_leading_slash() {
    let rule = FilterRule::exclude("test.txt").anchor_to_root();
    assert_eq!(rule.pattern(), "/test.txt");
}

#[test]
fn anchor_to_root_idempotent() {
    let rule = FilterRule::exclude("/test.txt").anchor_to_root();
    assert_eq!(rule.pattern(), "/test.txt");
}

#[test]
fn anchor_to_root_preserves_other_attributes() {
    let rule = FilterRule::exclude("test.txt")
        .with_perishable(true)
        .with_negate(true)
        .anchor_to_root();

    assert_eq!(rule.pattern(), "/test.txt");
    assert!(rule.is_perishable());
    assert!(rule.is_negated());
}

#[test]
fn anchor_to_root_with_wildcard() {
    let rule = FilterRule::exclude("*.bak").anchor_to_root();
    assert_eq!(rule.pattern(), "/*.bak");

    let rules = [rule, FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("test.bak"), false));
    assert!(set.allows(Path::new("subdir/test.bak"), false));
}

// =============================================================================
// Complex Anchoring Scenarios
// =============================================================================

#[test]
fn mixed_anchored_and_unanchored() {
    let rules = [
        FilterRule::exclude("/config.ini"), // Anchored
        FilterRule::exclude("*.log"),       // Unanchored
        FilterRule::exclude("/logs/"),      // Anchored directory
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Anchored file
    assert!(!set.allows(Path::new("config.ini"), false));
    assert!(set.allows(Path::new("subdir/config.ini"), false));

    // Unanchored extension
    assert!(!set.allows(Path::new("app.log"), false));
    assert!(!set.allows(Path::new("subdir/app.log"), false));

    // Anchored directory
    assert!(!set.allows(Path::new("logs"), true));
    assert!(set.allows(Path::new("subdir/logs"), true));
}

#[test]
fn anchored_vs_unanchored_same_name() {
    // Test precedence when both anchored and unanchored rules exist
    let rules = [
        FilterRule::include("/special.txt"), // Include at root
        FilterRule::exclude("special.txt"),  // Exclude everywhere else
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // First matching rule wins
    assert!(set.allows(Path::new("special.txt"), false));
    assert!(!set.allows(Path::new("subdir/special.txt"), false));
}

#[test]
fn anchored_with_nested_path() {
    let rules = [
        FilterRule::exclude("/a/b/c/test.txt"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Only exact path matches
    assert!(!set.allows(Path::new("a/b/c/test.txt"), false));
    assert!(set.allows(Path::new("test.txt"), false));
    assert!(set.allows(Path::new("b/c/test.txt"), false));
    assert!(set.allows(Path::new("x/a/b/c/test.txt"), false));
}

#[test]
fn double_star_anchored() {
    let rules = [
        FilterRule::exclude("/src/**/test.txt"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Anchored but ** allows nested matching under /src
    assert!(!set.allows(Path::new("src/test.txt"), false));
    assert!(!set.allows(Path::new("src/a/test.txt"), false));
    assert!(!set.allows(Path::new("src/a/b/c/test.txt"), false));
    // Not in /src
    assert!(set.allows(Path::new("test.txt"), false));
    assert!(set.allows(Path::new("other/src/test.txt"), false));
}

// =============================================================================
// Directory-Only Anchoring
// =============================================================================

#[test]
fn anchored_directory_only_pattern() {
    let rules = [
        FilterRule::exclude("/node_modules/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Only matches directory at root
    assert!(!set.allows(Path::new("node_modules"), true));
    assert!(!set.allows(Path::new("node_modules/package"), false));
    // Does not match file with same name
    assert!(set.allows(Path::new("node_modules"), false));
    // Does not match nested
    assert!(set.allows(Path::new("project/node_modules"), true));
}

#[test]
fn unanchored_directory_only_pattern() {
    let rules = [
        FilterRule::exclude("__pycache__/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Matches at any level
    assert!(!set.allows(Path::new("__pycache__"), true));
    assert!(!set.allows(Path::new("src/__pycache__"), true));
    assert!(!set.allows(Path::new("a/b/c/__pycache__"), true));
    // Still file-sensitive
    assert!(set.allows(Path::new("__pycache__"), false));
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn root_only_pattern() {
    let rules = [FilterRule::exclude("/"), FilterRule::include("**")];
    // This is a degenerate case - pattern is just "/"
    let set = FilterSet::from_rules(rules).unwrap();
    // Behavior may vary, but shouldn't panic
    let _ = set.allows(Path::new("anything"), false);
}

#[test]
fn double_leading_slash() {
    // Double slash should be normalized or handled gracefully
    let rule = FilterRule::exclude("//test.txt");
    assert_eq!(rule.pattern(), "//test.txt");
}

#[test]
fn anchored_empty_after_slash() {
    let rule = FilterRule::exclude("/");
    assert_eq!(rule.pattern(), "/");
}

#[test]
fn trailing_slash_does_not_anchor() {
    // Trailing slash makes it directory-only, not anchored
    let rules = [FilterRule::exclude("build/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Should match build directories at any level
    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("project/build"), true));
}

#[test]
fn multiple_slashes_in_pattern() {
    let rules = [FilterRule::exclude("a/b/c/"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Pattern with / is anchored
    assert!(!set.allows(Path::new("a/b/c"), true));
    assert!(set.allows(Path::new("x/a/b/c"), true));
}

// =============================================================================
// Real-World Scenarios
// =============================================================================

#[test]
fn rust_project_anchored_rules() {
    let rules = [
        // Anchored - only at project root
        FilterRule::exclude("/target/"),
        FilterRule::exclude("/Cargo.lock"),
        // Unanchored - anywhere in project
        FilterRule::exclude("*.rs.bk"),
        FilterRule::exclude(".DS_Store"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root target excluded
    assert!(!set.allows(Path::new("target"), true));
    assert!(!set.allows(Path::new("target/debug/binary"), false));
    // Nested target included (e.g., in a workspace member)
    assert!(set.allows(Path::new("crates/foo/target"), true));

    // Root Cargo.lock excluded
    assert!(!set.allows(Path::new("Cargo.lock"), false));
    // Nested Cargo.lock included
    assert!(set.allows(Path::new("crates/foo/Cargo.lock"), false));

    // Backup files excluded everywhere
    assert!(!set.allows(Path::new("main.rs.bk"), false));
    assert!(!set.allows(Path::new("src/lib.rs.bk"), false));
}

#[test]
fn web_project_mixed_anchoring() {
    let rules = [
        // Anchored to root
        FilterRule::exclude("/dist/"),
        FilterRule::exclude("/build/"),
        FilterRule::exclude("/.env"),
        // Unanchored
        FilterRule::exclude("node_modules/"),
        FilterRule::exclude("*.min.js"),
        FilterRule::exclude("*.min.css"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root dist/build excluded
    assert!(!set.allows(Path::new("dist"), true));
    assert!(!set.allows(Path::new("build"), true));
    // Nested not excluded
    assert!(set.allows(Path::new("packages/foo/dist"), true));

    // .env at root only
    assert!(!set.allows(Path::new(".env"), false));
    assert!(set.allows(Path::new("config/.env"), false));

    // node_modules anywhere
    assert!(!set.allows(Path::new("node_modules"), true));
    assert!(!set.allows(Path::new("packages/foo/node_modules"), true));

    // Minified files anywhere
    assert!(!set.allows(Path::new("app.min.js"), false));
    assert!(!set.allows(Path::new("dist/bundle.min.js"), false));
}

#[test]
fn monorepo_structure() {
    let rules = [
        // Root-level config files (anchored)
        FilterRule::include("/package.json"),
        FilterRule::include("/tsconfig.json"),
        FilterRule::include("/lerna.json"),
        // Per-package configs (unanchored)
        FilterRule::include("package.json"),
        FilterRule::include("tsconfig.json"),
        // Exclude build artifacts
        FilterRule::exclude("dist/"),
        FilterRule::exclude("node_modules/"),
        FilterRule::exclude("**"), // Default exclude
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root configs included
    assert!(set.allows(Path::new("package.json"), false));
    assert!(set.allows(Path::new("tsconfig.json"), false));
    assert!(set.allows(Path::new("lerna.json"), false));

    // Package configs included (unanchored pattern matched first due to ordering)
    assert!(set.allows(Path::new("packages/foo/package.json"), false));
    assert!(set.allows(Path::new("packages/bar/tsconfig.json"), false));
}
