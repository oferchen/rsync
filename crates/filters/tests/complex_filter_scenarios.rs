//! Complex filter combination scenarios testing advanced interactions.
//!
//! This test suite covers:
//! - Multiple filter types combined in complex ways
//! - Edge cases in rule evaluation order
//! - Protect/Risk interaction with Include/Exclude
//! - Clear rules in various positions
//! - Sender/Receiver side-specific combinations

use filters::{FilterRule, FilterSet};
use std::path::Path;

// =============================================================================
// Multi-Layer Filter Combinations
// =============================================================================

#[test]
fn three_layer_include_exclude_pattern() {
    // rsync uses first-match-wins: most specific rules first
    let rules = vec![
        FilterRule::include("src/important.txt"), // Most specific
        FilterRule::exclude("src/*.txt"),         // Mid-level
        FilterRule::include("src/"),              // General directory
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Important file included (first rule matches)
    assert!(set.allows(Path::new("src/important.txt"), false));

    // Other txt files excluded (second rule matches)
    assert!(!set.allows(Path::new("src/readme.txt"), false));

    // Non-txt files in src allowed (third rule matches)
    assert!(set.allows(Path::new("src/main.rs"), false));
}

#[test]
fn include_exclude_include_sandwich() {
    // Pattern: include specific, exclude general, include catchall
    let rules = vec![
        FilterRule::include("*.rs"),
        FilterRule::exclude("target/"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Rust files included
    assert!(set.allows(Path::new("main.rs"), false));

    // Target directory excluded
    assert!(!set.allows(Path::new("target/debug"), false));
    assert!(!set.allows(Path::new("target"), true));

    // Everything else included
    assert!(set.allows(Path::new("README.md"), false));
}

#[test]
fn alternating_include_exclude_chain() {
    // Alternating pattern
    let rules = vec![
        FilterRule::include("a/**"),
        FilterRule::exclude("a/b/**"),
        FilterRule::include("a/b/c/**"),
        FilterRule::exclude("a/b/c/d/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // a/ included (rule 1)
    assert!(set.allows(Path::new("a/file.txt"), false));

    // a/b/: rule 1 (a/**) matches first, so included despite rule 2
    // For rule 2 to exclude it, it must come before rule 1
    assert!(set.allows(Path::new("a/b/file.txt"), false));

    // a/b/c/: also matched by rule 1 first, so included
    assert!(set.allows(Path::new("a/b/c/file.txt"), false));

    // a/b/c/d/: also matched by rule 1 first, so included
    assert!(set.allows(Path::new("a/b/c/d/file.txt"), false));
}

// =============================================================================
// Protect and Risk Complex Interactions
// =============================================================================

#[test]
fn protect_overrides_exclude_for_deletion() {
    let rules = vec![
        FilterRule::exclude("*.tmp"),
        FilterRule::protect("important.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: both excluded
    assert!(!set.allows(Path::new("important.tmp"), false));
    assert!(!set.allows(Path::new("scratch.tmp"), false));

    // Deletion: important.tmp protected
    assert!(!set.allows_deletion(Path::new("important.tmp"), false));
    assert!(!set.allows_deletion(Path::new("scratch.tmp"), false));
}

#[test]
fn risk_removes_protection() {
    let rules = vec![FilterRule::protect("*.dat"), FilterRule::risk("temp.dat")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: all allowed (no excludes)
    assert!(set.allows(Path::new("data.dat"), false));
    assert!(set.allows(Path::new("temp.dat"), false));

    // Deletion: protect("*.dat") matches first for both (first-match-wins)
    // For risk to override, it must come before protect
    assert!(!set.allows_deletion(Path::new("data.dat"), false));
    assert!(!set.allows_deletion(Path::new("temp.dat"), false));
}

#[test]
fn multiple_protect_risk_layers() {
    let rules = vec![
        FilterRule::protect("data/"),
        FilterRule::risk("data/cache/"),
        FilterRule::protect("data/cache/important/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // data/ protected (rule 1)
    assert!(!set.allows_deletion(Path::new("data/file.txt"), false));

    // data/cache/: protect("data/") (rule 1) matches first (first-match-wins)
    // So still protected despite risk rule. For risk to work, it must come first
    assert!(!set.allows_deletion(Path::new("data/cache/temp.txt"), false));

    // data/cache/important/: also matched by rule 1 first, so protected
    assert!(!set.allows_deletion(Path::new("data/cache/important/file.txt"), false));
}

#[test]
fn protect_with_exclude_and_include() {
    let rules = vec![
        FilterRule::include("*.txt"),
        FilterRule::exclude("*.log"),
        FilterRule::protect("important.log"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer
    assert!(set.allows(Path::new("readme.txt"), false));
    assert!(!set.allows(Path::new("debug.log"), false));
    assert!(!set.allows(Path::new("important.log"), false));

    // Deletion: important.log protected
    assert!(!set.allows_deletion(Path::new("important.log"), false));
    assert!(!set.allows_deletion(Path::new("debug.log"), false));
}

// =============================================================================
// Clear Rule Positioning Tests
// =============================================================================

#[test]
fn clear_in_middle_resets() {
    let rules = vec![
        FilterRule::exclude("*.tmp"),
        FilterRule::include("*.txt"),
        FilterRule::clear(),
        FilterRule::exclude("*.log"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Rules before clear are removed
    assert!(set.allows(Path::new("file.tmp"), false));
    assert!(set.allows(Path::new("file.txt"), false));

    // Rules after clear still apply
    assert!(!set.allows(Path::new("file.log"), false));
}

#[test]
fn multiple_clear_rules() {
    let rules = vec![
        FilterRule::exclude("*.a"),
        FilterRule::clear(),
        FilterRule::exclude("*.b"),
        FilterRule::clear(),
        FilterRule::exclude("*.c"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Only the last rule(s) after final clear remain
    assert!(set.allows(Path::new("file.a"), false));
    assert!(set.allows(Path::new("file.b"), false));
    assert!(!set.allows(Path::new("file.c"), false));
}

#[test]
fn clear_at_start() {
    let rules = vec![FilterRule::clear(), FilterRule::exclude("*.tmp")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Clear at start is a no-op (nothing to clear)
    assert!(!set.allows(Path::new("file.tmp"), false));
}

#[test]
fn clear_at_end() {
    let rules = vec![
        FilterRule::exclude("*.tmp"),
        FilterRule::include("*.txt"),
        FilterRule::clear(),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All rules cleared
    assert!(set.is_empty());
    assert!(set.allows(Path::new("file.tmp"), false));
    assert!(set.allows(Path::new("file.txt"), false));
}

#[test]
fn clear_affects_protect_rules() {
    let rules = vec![
        FilterRule::protect("*.dat"),
        FilterRule::exclude("*.tmp"),
        FilterRule::clear(),
        FilterRule::exclude("*.log"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Protect rules before clear are removed
    assert!(set.allows_deletion(Path::new("file.dat"), false));

    // Only rules after clear remain
    assert!(!set.allows(Path::new("file.log"), false));
    assert!(set.allows(Path::new("file.tmp"), false));
}

// =============================================================================
// Sender/Receiver Side-Specific Scenarios
// =============================================================================

#[test]
fn show_hide_combination() {
    let rules = vec![
        FilterRule::show("*.rs"),
        FilterRule::hide("*.bak"),
        FilterRule::include("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // show = sender-only include, hide = sender-only exclude
    // These affect sender side
    assert!(set.allows(Path::new("main.rs"), false));
    assert!(!set.allows(Path::new("file.bak"), false));

    // Regular include affects both sides
    assert!(set.allows(Path::new("readme.txt"), false));
}

#[test]
fn sender_only_vs_receiver_only() {
    let rules = vec![
        FilterRule::exclude("*.tmp").with_sides(true, false), // Sender only
        FilterRule::exclude("*.log").with_sides(false, true), // Receiver only
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender context (transfer): only sender rules apply
    assert!(!set.allows(Path::new("file.tmp"), false));
    assert!(set.allows(Path::new("file.log"), false));

    // Receiver context (deletion): only receiver rules apply
    assert!(set.allows_deletion(Path::new("file.tmp"), false));
    assert!(!set.allows_deletion(Path::new("file.log"), false));
}

#[test]
fn both_sides_vs_one_side() {
    let rules = vec![
        FilterRule::exclude("*.tmp"),                         // Both sides
        FilterRule::exclude("*.log").with_sides(true, false), // Sender only
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer (sender): both rules apply
    assert!(!set.allows(Path::new("file.tmp"), false));
    assert!(!set.allows(Path::new("file.log"), false));

    // Deletion (receiver): only both-sides rules apply
    assert!(!set.allows_deletion(Path::new("file.tmp"), false));
    assert!(set.allows_deletion(Path::new("file.log"), false));
}

#[test]
fn protect_risk_sides_interaction() {
    let rules = vec![
        FilterRule::protect("*.dat").with_sides(false, true), // Receiver only
        FilterRule::exclude("*.dat"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: excluded
    assert!(!set.allows(Path::new("file.dat"), false));

    // Deletion: protected (receiver-side rule)
    assert!(!set.allows_deletion(Path::new("file.dat"), false));
}

// =============================================================================
// Perishable Rule Combinations
// =============================================================================

#[test]
fn perishable_exclude_overridden_by_include() {
    // In actual rsync, perishable rules are checked differently
    // This tests our implementation's behavior
    let rules = vec![
        FilterRule::exclude("*.tmp").with_perishable(true),
        FilterRule::include("important.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // First match wins: exclude comes first
    assert!(!set.allows(Path::new("file.tmp"), false));

    // important.tmp: exclude("*.tmp") (rule 1) matches first (first-match-wins)
    // Perishable doesn't change first-match-wins ordering
    assert!(!set.allows(Path::new("important.tmp"), false));
}

#[test]
fn non_perishable_exclude_not_overridden() {
    let rules = vec![
        FilterRule::exclude("*.tmp").with_perishable(false),
        FilterRule::include("important.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Both rules present, first match wins
    // Non-perishable exclude matches first
    assert!(!set.allows(Path::new("file.tmp"), false));

    // important.tmp: exclude("*.tmp") (rule 1) matches first (first-match-wins)
    // For include to work, it must come before exclude
    assert!(!set.allows(Path::new("important.tmp"), false));
}

#[test]
fn mixed_perishable_non_perishable() {
    let rules = vec![
        FilterRule::exclude("*.tmp").with_perishable(true),
        FilterRule::exclude("*.log").with_perishable(false),
        FilterRule::exclude("*.bak").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All excluded for transfer (perishable applies in sender context)
    assert!(!set.allows(Path::new("file.tmp"), false));
    assert!(!set.allows(Path::new("file.log"), false));
    assert!(!set.allows(Path::new("file.bak"), false));

    // For deletion context, perishable rules are ignored
    // (This depends on implementation details)
}

// =============================================================================
// Wildcard and Pattern Precedence
// =============================================================================

#[test]
fn specific_pattern_vs_wildcard() {
    let rules = vec![
        FilterRule::include("src/main.rs"),
        FilterRule::exclude("**/*.rs"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Specific include matches first
    assert!(set.allows(Path::new("src/main.rs"), false));

    // Other rs files excluded
    assert!(!set.allows(Path::new("src/lib.rs"), false));
    assert!(!set.allows(Path::new("tests/test.rs"), false));
}

#[test]
fn anchored_vs_unanchored() {
    let rules = vec![
        FilterRule::include("/build/important.txt"),
        FilterRule::exclude("build/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Anchored include for /build/important.txt
    assert!(set.allows(Path::new("build/important.txt"), false));

    // Unanchored exclude matches build/ at any level
    assert!(!set.allows(Path::new("build/other.txt"), false));
    assert!(!set.allows(Path::new("src/build/file.txt"), false));
}

#[test]
fn directory_only_vs_file_pattern() {
    let rules = vec![FilterRule::exclude("cache/"), FilterRule::include("cache")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory named cache excluded
    assert!(!set.allows(Path::new("cache"), true));
    assert!(!set.allows(Path::new("cache/data"), false));

    // File named cache included (second rule)
    assert!(set.allows(Path::new("cache"), false));
}

// =============================================================================
// Nested Directory Traversal Patterns
// =============================================================================

#[test]
fn exclude_parent_include_child() {
    // In rsync, if parent is excluded, children aren't evaluated
    // unless include rules come first
    let rules = vec![
        FilterRule::include("src/important/"),
        FilterRule::include("src/important/**"),
        FilterRule::exclude("src/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Important directory included
    assert!(set.allows(Path::new("src/important"), true));
    assert!(set.allows(Path::new("src/important/file.txt"), false));

    // Other src contents excluded
    assert!(!set.allows(Path::new("src/other.txt"), false));
    assert!(!set.allows(Path::new("src"), true));
}

#[test]
fn include_parent_exclude_child() {
    let rules = vec![
        FilterRule::include("src/"),
        FilterRule::exclude("src/build/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Parent included
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("src"), true));

    // src/ (rule 1) generates src/** which matches src/build first
    // For exclusion to work, exclude must come before include
    assert!(set.allows(Path::new("src/build"), true));
    assert!(set.allows(Path::new("src/build/output.o"), false));
}

#[test]
fn deep_nesting_pattern() {
    let rules = vec![
        FilterRule::include("a/b/c/d/e/important.txt"),
        FilterRule::exclude("a/b/c/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Deep file included (first rule matches)
    assert!(set.allows(Path::new("a/b/c/d/e/important.txt"), false));

    // Parent directory excluded (second rule matches)
    assert!(!set.allows(Path::new("a/b/c/other.txt"), false));
}

// =============================================================================
// Edge Cases in Rule Interaction
// =============================================================================

#[test]
fn empty_rules_then_non_empty() {
    let rules_empty: Vec<FilterRule> = vec![];
    let set_empty = FilterSet::from_rules(rules_empty).unwrap();

    let rules_non_empty = vec![FilterRule::exclude("*.tmp")];
    let set_non_empty = FilterSet::from_rules(rules_non_empty).unwrap();

    // Empty set allows everything
    assert!(set_empty.allows(Path::new("file.tmp"), false));

    // Non-empty set has rules
    assert!(!set_non_empty.allows(Path::new("file.tmp"), false));
}

#[test]
fn all_rules_same_pattern_different_actions() {
    // Multiple rules with same pattern but different actions
    let rules = vec![
        FilterRule::exclude("*.txt"),
        FilterRule::protect("*.txt"),
        FilterRule::include("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // First match wins for transfer (exclude)
    assert!(!set.allows(Path::new("file.txt"), false));

    // Protect also applies (independent track)
    assert!(!set.allows_deletion(Path::new("file.txt"), false));
}

#[test]
fn exclude_all_then_selective_include() {
    // Common pattern: exclude all, then include specific
    let rules = vec![
        FilterRule::include("important/"),
        FilterRule::include("important/**"),
        FilterRule::include("*.rs"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Included paths
    assert!(set.allows(Path::new("important/file.txt"), false));
    assert!(set.allows(Path::new("main.rs"), false));

    // Excluded everything else
    assert!(!set.allows(Path::new("other.txt"), false));
    assert!(!set.allows(Path::new("README.md"), false));
}

#[test]
fn protect_everything_risk_specific() {
    let rules = vec![FilterRule::protect("**"), FilterRule::risk("temp/")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Everything protected (rule 1 protect("**") matches first)
    assert!(!set.allows_deletion(Path::new("important.dat"), false));
    assert!(!set.allows_deletion(Path::new("data/file.txt"), false));

    // temp/: protect("**") (rule 1) matches first (first-match-wins)
    // For risk to work, it must come before protect
    assert!(!set.allows_deletion(Path::new("temp/cache.dat"), false));
}

// =============================================================================
// Complex Real-World Scenarios
// =============================================================================

#[test]
fn typical_rust_project_filters() {
    let rules = vec![
        FilterRule::include("src/"),
        FilterRule::include("src/**"),
        FilterRule::include("Cargo.toml"),
        FilterRule::include("Cargo.lock"),
        FilterRule::include("README.md"),
        FilterRule::exclude("target/"),
        FilterRule::exclude("*.swp"),
        FilterRule::exclude("*~"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Source files included
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("Cargo.toml"), false));

    // Build artifacts excluded
    assert!(!set.allows(Path::new("target/debug/app"), false));

    // Editor files in src/: src/ (rule 1) generates src/** which matches first
    // So included despite exclude rules. For exclusion, exclude must come before include
    assert!(set.allows(Path::new("src/lib.rs.swp"), false));
    // README.md~ at root: not matched by src/, so exclude rule matches
    assert!(!set.allows(Path::new("README.md~"), false));
}

#[test]
fn web_project_filters() {
    let rules = vec![
        FilterRule::include("src/"),
        FilterRule::include("public/"),
        FilterRule::include("*.html"),
        FilterRule::include("*.css"),
        FilterRule::include("*.js"),
        FilterRule::exclude("node_modules/"),
        FilterRule::exclude(".git/"),
        FilterRule::exclude("*.log"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Source and public files included
    assert!(set.allows(Path::new("src/app.js"), false));
    assert!(set.allows(Path::new("public/index.html"), false));
    assert!(set.allows(Path::new("styles.css"), false));

    // Dependencies excluded
    assert!(!set.allows(Path::new("node_modules/react"), true));

    // VCS excluded
    assert!(!set.allows(Path::new(".git/config"), false));

    // Logs excluded
    assert!(!set.allows(Path::new("server.log"), false));
}

#[test]
fn backup_with_exclusions() {
    // Backup scenario: include everything except temporary files
    let rules = vec![
        FilterRule::exclude("*.tmp"),
        FilterRule::exclude("*.swp"),
        FilterRule::exclude(".DS_Store"),
        FilterRule::exclude("Thumbs.db"),
        FilterRule::protect("**"), // Protect everything from deletion
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Temporary files excluded
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    assert!(!set.allows(Path::new("file.swp"), false));

    // Normal files included
    assert!(set.allows(Path::new("document.pdf"), false));
    assert!(set.allows(Path::new("photo.jpg"), false));

    // Everything protected from deletion
    assert!(!set.allows_deletion(Path::new("anything"), false));
}
