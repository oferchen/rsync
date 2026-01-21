//! Integration tests for filter rule precedence and merging.
//!
//! These tests verify that filter rules are evaluated in the correct order
//! using rsync's **first-match-wins** semantics. Rules are checked in order
//! from first to last, and the first matching rule determines the outcome.
//!
//! This means:
//! - Specific exceptions must come BEFORE general rules
//! - To include a specific file while excluding a pattern, put the include first
//!
//! Reference: rsync 3.4.1 exclude.c lines 1043-1065 for check_filter.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Basic Precedence Tests
// ============================================================================

/// Verifies first-match-wins: specific includes must come before general excludes.
///
/// From rsync man page: "The include/exclude rules are checked in the
/// order of definition. The first matching rule is used."
#[test]
fn first_matching_rule_wins() {
    let rules = [
        FilterRule::include("important.txt"),
        FilterRule::exclude("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Earlier include takes precedence
    assert!(set.allows(Path::new("important.txt"), false));

    // Other .txt files excluded by second rule
    assert!(!set.allows(Path::new("other.txt"), false));
}

/// Verifies exclude before include for exceptions.
#[test]
fn exclude_before_include() {
    let rules = [
        FilterRule::exclude("secret.txt"),
        FilterRule::include("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // secret.txt excluded by first rule
    assert!(!set.allows(Path::new("secret.txt"), false));

    // Other .txt files included by second rule
    assert!(set.allows(Path::new("readme.txt"), false));
}

/// Verifies multiple rules with first-match-wins.
#[test]
fn alternating_rules() {
    // With first-match-wins, order specific rules before general ones
    let rules = [
        FilterRule::include("temp.txt"), // Most specific: include temp.txt
        FilterRule::include("*.txt"),    // Include other .txt files
        FilterRule::exclude("*"),        // Exclude everything else
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // temp.txt is included by first rule
    assert!(set.allows(Path::new("temp.txt"), false));

    // Other .txt files included by second rule
    assert!(set.allows(Path::new("readme.txt"), false));

    // Non-.txt files excluded by third rule
    assert!(!set.allows(Path::new("image.png"), false));
}

// ============================================================================
// Specificity Tests
// ============================================================================

/// Verifies that more specific patterns take precedence when first.
#[test]
fn specific_pattern_first() {
    let rules = [
        FilterRule::exclude("test_*.rs"), // Most specific first
        FilterRule::include("*.rs"),      // Then general include
        FilterRule::exclude("*"),         // Finally catch-all exclude
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // test_*.rs files excluded by first rule
    assert!(!set.allows(Path::new("test_main.rs"), false));

    // Regular .rs files included by second rule
    assert!(set.allows(Path::new("main.rs"), false));

    // Non-.rs files excluded by third rule
    assert!(!set.allows(Path::new("Cargo.toml"), false));
}

/// Verifies that specific patterns must come before general ones.
#[test]
fn general_pattern_before_specific() {
    let rules = [
        FilterRule::include("*.txt"),         // General (comes first)
        FilterRule::exclude("important.txt"), // Specific (comes second, but won't match)
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Important.txt is included because *.txt matches first
    assert!(set.allows(Path::new("important.txt"), false));
}

/// Verifies directory patterns vs file patterns.
#[test]
fn directory_vs_file_pattern() {
    let rules = [
        FilterRule::include("build/"), // Directory pattern first
        FilterRule::exclude("build"),  // Then file/directory pattern
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory named build is included (first rule matches)
    assert!(set.allows(Path::new("build"), true));

    // File named build is excluded (first rule doesn't match file, second does)
    assert!(!set.allows(Path::new("build"), false));
}

// ============================================================================
// Complex Precedence Scenarios
// ============================================================================

/// Verifies complex exclude/include chain.
#[test]
fn complex_chain() {
    // With first-match-wins, most specific rules come first
    let rules = [
        FilterRule::include("src/**/test/fixtures/**"), // Most specific: fixtures
        FilterRule::exclude("src/**/test/**"),          // Then: exclude test dirs
        FilterRule::include("src/**"),                  // Then: include src
        FilterRule::exclude("*"),                       // Finally: exclude all
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // fixtures within test included (first rule)
    assert!(set.allows(Path::new("src/lib/test/fixtures/data.json"), false));

    // test directories excluded (second rule)
    assert!(!set.allows(Path::new("src/lib/test/unit.rs"), false));

    // src files included (third rule)
    assert!(set.allows(Path::new("src/main.rs"), false));

    // Root files excluded (fourth rule)
    assert!(!set.allows(Path::new("Cargo.toml"), false));
}

/// Verifies multiple wildcard patterns interact correctly.
#[test]
fn multiple_wildcard_patterns() {
    // With first-match-wins, most specific rules come first
    let rules = [
        FilterRule::include("**/old/critical.log"), // Most specific: critical log
        FilterRule::exclude("**/old/*.log"),        // Then: exclude old logs
        FilterRule::include("**/important.log"),    // Then: include important
        FilterRule::exclude("**/*.log"),            // Finally: exclude all logs
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Critical old log included (first rule)
    assert!(set.allows(Path::new("old/critical.log"), false));

    // Other old logs excluded (second rule)
    assert!(!set.allows(Path::new("old/archive.log"), false));

    // Important log included (third rule)
    assert!(set.allows(Path::new("important.log"), false));

    // Regular logs excluded (fourth rule)
    assert!(!set.allows(Path::new("app.log"), false));
    assert!(!set.allows(Path::new("debug/trace.log"), false));
}

/// Verifies anchored vs unanchored precedence.
#[test]
fn anchored_vs_unanchored() {
    let rules = [
        FilterRule::include("/build"), // Anchored - only at root (first)
        FilterRule::exclude("build"),  // Unanchored - matches anywhere (second)
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Root build is included (anchored rule matches first)
    assert!(set.allows(Path::new("build"), false));

    // Nested build is excluded (anchored rule doesn't match, unanchored does)
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

/// Verifies risk overrides protect with first-match-wins.
#[test]
fn risk_overrides_protect() {
    // With first-match-wins, risk must come before protect
    let rules = [
        FilterRule::risk("data/temp/**"), // Risk for temp first
        FilterRule::protect("data/**"),   // Protect data second
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // temp within data is not protected (risk matches first)
    assert!(set.allows_deletion(Path::new("data/temp/scratch.dat"), false));

    // Other data is protected (protect matches)
    assert!(!set.allows_deletion(Path::new("data/important.dat"), false));
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
    // With first-match-wins, includes/shows come before exclude
    let rules = [
        FilterRule::show("visible/**"),   // Show visible (sender-only)
        FilterRule::include("always/**"), // Include always (both sides)
        FilterRule::exclude("*"),         // Exclude all
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
    // With first-match-wins, include must come before exclude
    let rules = [
        FilterRule::exclude("*"),
        FilterRule::clear(),
        FilterRule::include("important.bak"), // Include specific first
        FilterRule::exclude("*.bak"),         // Then exclude pattern
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Old exclude * is cleared
    assert!(set.allows(Path::new("file.txt"), false));

    // New precedence: important.bak included (first rule after clear)
    assert!(set.allows(Path::new("important.bak"), false));

    // Other .bak excluded (second rule after clear)
    assert!(!set.allows(Path::new("backup.bak"), false));
}

// ============================================================================
// Duplicate and Overlapping Rules
// ============================================================================

/// Verifies duplicate rules are handled.
#[test]
fn duplicate_rules() {
    // With first-match-wins, include must come first
    let rules = [
        FilterRule::include("keep.tmp"),
        FilterRule::exclude("*.tmp"),
        FilterRule::exclude("*.tmp"), // Duplicate (harmless)
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // keep.tmp included by first rule
    assert!(set.allows(Path::new("keep.tmp"), false));

    // Other .tmp excluded
    assert!(!set.allows(Path::new("scratch.tmp"), false));
}

/// Verifies overlapping patterns.
#[test]
fn overlapping_patterns() {
    // With first-match-wins, specific include must come first
    let rules = [
        FilterRule::include("readme.txt"),
        FilterRule::exclude("*.txt"),
        FilterRule::exclude("readme.*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // readme.txt included by first rule
    assert!(set.allows(Path::new("readme.txt"), false));

    // Other txt files excluded
    assert!(!set.allows(Path::new("notes.txt"), false));

    // Other readme files excluded
    assert!(!set.allows(Path::new("readme.md"), false));
}

/// Verifies subset patterns.
#[test]
fn subset_patterns() {
    // With first-match-wins, more specific (include) comes first
    let rules = [
        FilterRule::include("src/**/*.rs"), // Include src .rs files first
        FilterRule::exclude("**/*.rs"),     // Then exclude all .rs files
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // src .rs files included (first rule matches)
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("src/lib/util.rs"), false));

    // Other .rs files excluded (second rule matches)
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
    // With first-match-wins, include comes first
    let rules = [
        FilterRule::include("keep/**"),
        FilterRule::exclude("*").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: keep/** matches first, so included
    assert!(set.allows(Path::new("keep/file.txt"), false));

    // Deletion: keep/** matches first (perishable doesn't affect include rules)
    assert!(set.allows_deletion(Path::new("keep/file.txt"), false));

    // Other files: excluded from transfer (perishable exclude matches)
    assert!(!set.allows(Path::new("other.txt"), false));

    // For deletion: perishable exclude is skipped, so no rule matches,
    // defaults to include (transfer_allowed = true), so deletable
    assert!(set.allows_deletion(Path::new("other.txt"), false));
}
