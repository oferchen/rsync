//! Integration tests for filter rule precedence and merging.
//!
//! These tests verify that filter rules are evaluated in the correct order
//! and that the "last matching rule wins" semantics are correctly implemented.
//! This matches rsync's behavior where rules are checked in order and the
//! first matching rule determines the outcome.
//!
//! Reference: rsync 3.4.1 exclude.c lines 1043-1065 for check_filter.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Basic Precedence Tests
// ============================================================================

/// Verifies that later rules override earlier rules.
///
/// From rsync man page: "The include/exclude rules are checked in the
/// order of definition. The first matching rule is used."
#[test]
fn last_matching_rule_wins() {
    let rules = [
        FilterRule::exclude("*.txt"),
        FilterRule::include("important.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Later include overrides earlier exclude
    assert!(set.allows(Path::new("important.txt"), false));

    // Non-matching include doesn't affect other .txt files
    assert!(!set.allows(Path::new("other.txt"), false));
}

/// Verifies exclude after include.
#[test]
fn exclude_after_include() {
    let rules = [
        FilterRule::include("*.txt"),
        FilterRule::exclude("secret.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Most .txt files included
    assert!(set.allows(Path::new("readme.txt"), false));

    // But secret.txt excluded by later rule
    assert!(!set.allows(Path::new("secret.txt"), false));
}

/// Verifies multiple alternating rules.
#[test]
fn alternating_rules() {
    let rules = [
        FilterRule::exclude("*"),         // Exclude everything
        FilterRule::include("*.txt"),     // Include .txt
        FilterRule::exclude("temp.txt"),  // Exclude temp.txt
        FilterRule::include("temp.txt"),  // Include temp.txt again
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // temp.txt is included by last matching rule
    assert!(set.allows(Path::new("temp.txt"), false));

    // Other .txt files included
    assert!(set.allows(Path::new("readme.txt"), false));

    // Non-.txt files excluded
    assert!(!set.allows(Path::new("image.png"), false));
}

// ============================================================================
// Specificity Tests
// ============================================================================

/// Verifies that more specific patterns take precedence when last.
#[test]
fn specific_pattern_last() {
    let rules = [
        FilterRule::exclude("*"),
        FilterRule::include("*.rs"),
        FilterRule::exclude("test_*.rs"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Regular .rs files included
    assert!(set.allows(Path::new("main.rs"), false));

    // test_*.rs files excluded
    assert!(!set.allows(Path::new("test_main.rs"), false));

    // Non-.rs files excluded
    assert!(!set.allows(Path::new("Cargo.toml"), false));
}

/// Verifies that less specific patterns can override more specific ones.
#[test]
fn general_pattern_after_specific() {
    let rules = [
        FilterRule::exclude("important.txt"),  // Specific
        FilterRule::include("*.txt"),          // General (comes last)
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Important.txt is included because *.txt comes last
    assert!(set.allows(Path::new("important.txt"), false));
}

/// Verifies directory patterns vs file patterns.
#[test]
fn directory_vs_file_pattern() {
    let rules = [
        FilterRule::exclude("build"),    // Matches file or directory named build
        FilterRule::include("build/"),   // Matches only directory named build
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory named build is included (last matching rule)
    assert!(set.allows(Path::new("build"), true));

    // File named build is excluded (first rule matches, second doesn't)
    assert!(!set.allows(Path::new("build"), false));
}

// ============================================================================
// Complex Precedence Scenarios
// ============================================================================

/// Verifies complex exclude/include chain.
#[test]
fn complex_chain() {
    let rules = [
        FilterRule::exclude("*"),
        FilterRule::include("src/**"),
        FilterRule::exclude("src/**/test/**"),
        FilterRule::include("src/**/test/fixtures/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root files excluded
    assert!(!set.allows(Path::new("Cargo.toml"), false));

    // src files included
    assert!(set.allows(Path::new("src/main.rs"), false));

    // test directories excluded
    assert!(!set.allows(Path::new("src/lib/test/unit.rs"), false));

    // But fixtures within test included
    assert!(set.allows(Path::new("src/lib/test/fixtures/data.json"), false));
}

/// Verifies multiple wildcard patterns interact correctly.
#[test]
fn multiple_wildcard_patterns() {
    let rules = [
        FilterRule::exclude("**/*.log"),
        FilterRule::include("**/important.log"),
        FilterRule::exclude("**/old/*.log"),
        FilterRule::include("**/old/critical.log"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Regular logs excluded
    assert!(!set.allows(Path::new("app.log"), false));
    assert!(!set.allows(Path::new("debug/trace.log"), false));

    // Important log included
    assert!(set.allows(Path::new("important.log"), false));

    // Old logs excluded
    assert!(!set.allows(Path::new("old/archive.log"), false));

    // Critical old log included
    assert!(set.allows(Path::new("old/critical.log"), false));
}

/// Verifies anchored vs unanchored precedence.
#[test]
fn anchored_vs_unanchored() {
    let rules = [
        FilterRule::exclude("build"),     // Unanchored - matches anywhere
        FilterRule::include("/build"),    // Anchored - only at root
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root build is included (anchored rule matches and comes last)
    assert!(set.allows(Path::new("build"), false));

    // Nested build is excluded (only unanchored rule matches)
    assert!(!set.allows(Path::new("src/build"), false));
}

// ============================================================================
// Rule Type Interaction Tests
// ============================================================================

/// Verifies include/exclude separate from protect/risk.
#[test]
fn include_exclude_separate_from_protect_risk() {
    let rules = [
        FilterRule::exclude("secret.txt"),
        FilterRule::protect("secret.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Excluded from transfer (include/exclude decision)
    assert!(!set.allows(Path::new("secret.txt"), false));

    // But protected from deletion (protect/risk decision)
    assert!(!set.allows_deletion(Path::new("secret.txt"), false));
}

/// Verifies protect and exclude on same file.
#[test]
fn protect_and_exclude_same_file() {
    let rules = [
        FilterRule::exclude("*.bak"),
        FilterRule::protect("important.bak"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All .bak excluded from transfer
    assert!(!set.allows(Path::new("important.bak"), false));
    assert!(!set.allows(Path::new("other.bak"), false));

    // Only important.bak protected
    assert!(!set.allows_deletion(Path::new("important.bak"), false));
    assert!(!set.allows_deletion(Path::new("other.bak"), false));
}

/// Verifies risk undoes protect.
#[test]
fn risk_undoes_protect() {
    let rules = [
        FilterRule::protect("data/**"),
        FilterRule::risk("data/temp/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Data is protected
    assert!(!set.allows_deletion(Path::new("data/important.dat"), false));

    // But temp within data is not
    assert!(set.allows_deletion(Path::new("data/temp/scratch.dat"), false));
}

// ============================================================================
// Side-Specific Precedence
// ============================================================================

/// Verifies sender-only and receiver-only rules don't interfere.
#[test]
fn sender_receiver_independence() {
    let rules = [
        FilterRule::exclude("sender.txt").with_sides(true, false),
        FilterRule::exclude("receiver.txt").with_sides(false, true),
        FilterRule::include("both.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender checks (allows())
    assert!(!set.allows(Path::new("sender.txt"), false));
    assert!(set.allows(Path::new("receiver.txt"), false)); // Receiver rule doesn't apply
    assert!(set.allows(Path::new("both.txt"), false));

    // Receiver checks (allows_deletion())
    assert!(set.allows_deletion(Path::new("sender.txt"), false)); // Sender rule doesn't apply
    assert!(!set.allows_deletion(Path::new("receiver.txt"), false));
    assert!(set.allows_deletion(Path::new("both.txt"), false));
}

/// Verifies show/hide vs include/exclude precedence.
#[test]
fn show_hide_vs_include_exclude() {
    let rules = [
        FilterRule::exclude("*"),           // Exclude all
        FilterRule::show("visible/**"),     // Show visible (sender-only)
        FilterRule::include("always/**"),   // Include always (both sides)
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Both visible and always are included for transfer
    assert!(set.allows(Path::new("visible/file"), false));
    assert!(set.allows(Path::new("always/file"), false));

    // Other files excluded
    assert!(!set.allows(Path::new("other/file"), false));
}

// ============================================================================
// Clear Rule Precedence
// ============================================================================

/// Verifies clear resets precedence.
#[test]
fn clear_resets_precedence() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::include("important.tmp"),
        FilterRule::clear(),
        FilterRule::exclude("*.log"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Old rules cleared
    assert!(set.allows(Path::new("scratch.tmp"), false));
    assert!(set.allows(Path::new("important.tmp"), false));

    // New rule active
    assert!(!set.allows(Path::new("debug.log"), false));
}

/// Verifies rules after clear establish new precedence.
#[test]
fn new_precedence_after_clear() {
    let rules = [
        FilterRule::exclude("*"),
        FilterRule::clear(),
        FilterRule::exclude("*.bak"),
        FilterRule::include("important.bak"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Old exclude * is cleared
    assert!(set.allows(Path::new("file.txt"), false));

    // New precedence: important.bak included
    assert!(set.allows(Path::new("important.bak"), false));

    // Other .bak excluded
    assert!(!set.allows(Path::new("backup.bak"), false));
}

// ============================================================================
// Duplicate and Overlapping Rules
// ============================================================================

/// Verifies duplicate rules are handled.
#[test]
fn duplicate_rules() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::exclude("*.tmp"), // Duplicate
        FilterRule::include("keep.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Behavior is same as without duplicate
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    assert!(set.allows(Path::new("keep.tmp"), false));
}

/// Verifies overlapping patterns.
#[test]
fn overlapping_patterns() {
    let rules = [
        FilterRule::exclude("*.txt"),
        FilterRule::exclude("readme.*"),
        FilterRule::include("readme.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // readme.txt matches multiple exclude patterns but include comes last
    assert!(set.allows(Path::new("readme.txt"), false));

    // Other readme files excluded
    assert!(!set.allows(Path::new("readme.md"), false));

    // Other txt files excluded
    assert!(!set.allows(Path::new("notes.txt"), false));
}

/// Verifies subset patterns.
#[test]
fn subset_patterns() {
    let rules = [
        FilterRule::exclude("**/*.rs"),      // All .rs files
        FilterRule::include("src/**/*.rs"),  // But include src .rs files
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // src .rs files included
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("src/lib/util.rs"), false));

    // Other .rs files excluded
    assert!(!set.allows(Path::new("tests/test.rs"), false));
    assert!(!set.allows(Path::new("main.rs"), false));
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies no matching rule defaults to include.
#[test]
fn no_match_defaults_to_include() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();

    // Non-matching path is allowed
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(set.allows_deletion(Path::new("file.txt"), false));
}

/// Verifies single rule filter set.
#[test]
fn single_rule_precedence() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();

    assert!(!set.allows(Path::new("file.bak"), false));
    assert!(set.allows(Path::new("file.txt"), false));
}

/// Verifies all-include filter set.
#[test]
fn all_include_rules() {
    let rules = [
        FilterRule::include("*.txt"),
        FilterRule::include("*.rs"),
        FilterRule::include("*.md"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All matching patterns included
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(set.allows(Path::new("file.rs"), false));
    assert!(set.allows(Path::new("file.md"), false));

    // Non-matching also included (default)
    assert!(set.allows(Path::new("file.py"), false));
}

/// Verifies all-exclude filter set.
#[test]
fn all_exclude_rules() {
    let rules = [
        FilterRule::exclude("*.txt"),
        FilterRule::exclude("*.rs"),
        FilterRule::exclude("*.md"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All matching patterns excluded
    assert!(!set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("file.rs"), false));
    assert!(!set.allows(Path::new("file.md"), false));

    // Non-matching allowed (default)
    assert!(set.allows(Path::new("file.py"), false));
}

/// Verifies perishable rules in precedence chain.
#[test]
fn perishable_in_precedence() {
    let rules = [
        FilterRule::exclude("*").with_perishable(true),
        FilterRule::include("keep/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: include overrides perishable exclude
    assert!(set.allows(Path::new("keep/file.txt"), false));

    // Deletion: perishable exclude is skipped during deletion checks,
    // so keep/** still matches and file is included (transfer_allowed = true).
    // Since it's included and not protected, it IS deletable.
    assert!(set.allows_deletion(Path::new("keep/file.txt"), false));

    // Other files: excluded from transfer (perishable applies on sender)
    assert!(!set.allows(Path::new("other.txt"), false));

    // For deletion: perishable exclude is skipped, so no rule matches,
    // defaults to include (transfer_allowed = true), so deletable
    assert!(set.allows_deletion(Path::new("other.txt"), false));
}
