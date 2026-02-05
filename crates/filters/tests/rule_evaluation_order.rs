//! Tests for filter rule evaluation order matching upstream rsync behavior.
//!
//! Rsync uses **first-match-wins** semantics for filter rules. Rules are evaluated
//! in the order they are specified, and the first rule that matches determines
//! the outcome.
//!
//! Key behaviors verified:
//! - Rules are evaluated in definition order (first to last)
//! - First matching rule determines the outcome
//! - No rule matching defaults to include
//! - Each rule type (include/exclude/protect/risk) follows the same order semantics
//!
//! Reference: rsync 3.4.1 exclude.c `check_filter()` function which iterates
//! rules in order and returns on first match.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// =============================================================================
// First-Match-Wins Fundamental Behavior
// =============================================================================

/// Verifies that the first matching rule wins, not the most specific or last.
#[test]
fn first_match_wins_exclude_then_include() {
    // Exclude rule comes first - it should win
    let rules = [
        FilterRule::exclude("*.txt"),
        FilterRule::include("important.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // important.txt matches exclude first, so excluded
    assert!(!set.allows(Path::new("important.txt"), false));
    assert!(!set.allows(Path::new("any.txt"), false));
}

/// Verifies that include-before-exclude creates proper exceptions.
#[test]
fn first_match_wins_include_then_exclude() {
    // Include rule comes first - it should win for matching paths
    let rules = [
        FilterRule::include("important.txt"),
        FilterRule::exclude("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // important.txt matches include first, so included
    assert!(set.allows(Path::new("important.txt"), false));
    // other.txt matches exclude (include rule doesn't match)
    assert!(!set.allows(Path::new("other.txt"), false));
}

/// Verifies that rule position determines outcome when patterns overlap.
#[test]
fn position_determines_outcome_for_overlapping_patterns() {
    // Both patterns match "test.txt" - first wins
    let rules_exclude_first = [FilterRule::exclude("*.txt"), FilterRule::include("*")];
    let set1 = FilterSet::from_rules(rules_exclude_first).unwrap();
    assert!(!set1.allows(Path::new("test.txt"), false));

    // Reverse order - include wins
    let rules_include_first = [FilterRule::include("*"), FilterRule::exclude("*.txt")];
    let set2 = FilterSet::from_rules(rules_include_first).unwrap();
    assert!(set2.allows(Path::new("test.txt"), false));
}

// =============================================================================
// Order-Sensitive Exception Patterns
// =============================================================================

/// Classic rsync pattern: include specific, exclude general.
#[test]
fn classic_exception_pattern() {
    // This is the recommended way to create exceptions in rsync
    let rules = [
        FilterRule::include("keep.log"), // Exception first
        FilterRule::exclude("*.log"),    // General exclusion second
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("keep.log"), false));
    assert!(!set.allows(Path::new("debug.log"), false));
    assert!(!set.allows(Path::new("error.log"), false));
}

/// Incorrect order: exception after general rule has no effect.
#[test]
fn exception_after_general_rule_ignored() {
    // This is the WRONG way - exception has no effect
    let rules = [
        FilterRule::exclude("*.log"),    // General exclusion first
        FilterRule::include("keep.log"), // Exception second - never reached!
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // keep.log matches *.log first, so it's excluded
    assert!(!set.allows(Path::new("keep.log"), false));
    assert!(!set.allows(Path::new("debug.log"), false));
}

/// Multiple exceptions must all come before the general rule.
#[test]
fn multiple_exceptions_before_general() {
    let rules = [
        FilterRule::include("error.log"),
        FilterRule::include("audit.log"),
        FilterRule::include("security.log"),
        FilterRule::exclude("*.log"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("error.log"), false));
    assert!(set.allows(Path::new("audit.log"), false));
    assert!(set.allows(Path::new("security.log"), false));
    assert!(!set.allows(Path::new("debug.log"), false));
    assert!(!set.allows(Path::new("app.log"), false));
}

/// Verifies that the exception order matters too.
#[test]
fn exception_order_among_exceptions() {
    // First exception rule determines outcome when multiple match
    let rules = [
        FilterRule::exclude("important.txt"), // Exclude this specific file
        FilterRule::include("*.txt"),         // But generally include .txt
        FilterRule::exclude("*"),             // Catch-all exclude
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // important.txt excluded by first rule
    assert!(!set.allows(Path::new("important.txt"), false));
    // readme.txt included by second rule
    assert!(set.allows(Path::new("readme.txt"), false));
    // other files excluded by third rule
    assert!(!set.allows(Path::new("image.png"), false));
}

// =============================================================================
// Sequential Evaluation Tests
// =============================================================================

/// Verifies that evaluation stops at first match.
#[test]
fn evaluation_stops_at_first_match() {
    // Even with many rules, only the first match matters
    let rules = [
        FilterRule::include("file.txt"),
        FilterRule::exclude("file.txt"),
        FilterRule::include("file.txt"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // First rule wins - file is included
    assert!(set.allows(Path::new("file.txt"), false));
}

/// Verifies that non-matching rules are skipped.
#[test]
fn non_matching_rules_skipped() {
    let rules = [
        FilterRule::exclude("*.rs"),
        FilterRule::exclude("*.py"),
        FilterRule::exclude("*.js"),
        FilterRule::include("*.txt"),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // data.txt: first three rules don't match, fourth matches (include)
    assert!(set.allows(Path::new("data.txt"), false));
    // main.rs: first rule matches (exclude)
    assert!(!set.allows(Path::new("main.rs"), false));
    // image.png: first four rules don't match, fifth matches (exclude)
    assert!(!set.allows(Path::new("image.png"), false));
}

/// Verifies linear search through rules.
#[test]
fn linear_search_through_rules() {
    // Create a specific pattern where order determines outcome
    let rules = [
        FilterRule::include("a"),
        FilterRule::include("b"),
        FilterRule::exclude("c"),
        FilterRule::include("c"), // Never reached for "c"
        FilterRule::include("d"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("a"), false));
    assert!(set.allows(Path::new("b"), false));
    assert!(!set.allows(Path::new("c"), false)); // Excluded by rule 3
    assert!(set.allows(Path::new("d"), false));
}

// =============================================================================
// No Match Default Behavior
// =============================================================================

/// Verifies that no matching rule defaults to include.
#[test]
fn no_match_defaults_to_include() {
    let rules = [FilterRule::exclude("*.log"), FilterRule::exclude("*.tmp")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Neither rule matches, default is include
    assert!(set.allows(Path::new("readme.txt"), false));
    assert!(set.allows(Path::new("src/main.rs"), false));
}

/// Empty filter set allows everything.
#[test]
fn empty_rules_allow_all() {
    let rules: Vec<FilterRule> = vec![];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("anything.txt"), false));
    assert!(set.allows(Path::new("deeply/nested/file.bin"), false));
    assert!(set.allows(Path::new(".hidden"), false));
}

/// All-exclude rules: non-matching paths still allowed.
#[test]
fn all_exclude_rules_non_matching_allowed() {
    let rules = [
        FilterRule::exclude("*.log"),
        FilterRule::exclude("*.tmp"),
        FilterRule::exclude("*.bak"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("source.rs"), false));
    assert!(set.allows(Path::new("config.toml"), false));
}

// =============================================================================
// Protect/Risk Order Evaluation
// =============================================================================

/// Verifies protect rules follow first-match-wins.
#[test]
fn protect_first_match_wins() {
    // Risk before protect: risk wins
    let rules_risk_first = [
        FilterRule::risk("data/temp/**"),
        FilterRule::protect("data/**"),
    ];
    let set1 = FilterSet::from_rules(rules_risk_first).unwrap();
    assert!(set1.allows_deletion(Path::new("data/temp/cache.dat"), false));
    assert!(!set1.allows_deletion(Path::new("data/important.dat"), false));

    // Protect before risk: protect wins
    let rules_protect_first = [
        FilterRule::protect("data/**"),
        FilterRule::risk("data/temp/**"),
    ];
    let set2 = FilterSet::from_rules(rules_protect_first).unwrap();
    // protect("data/**") matches first, so protected
    assert!(!set2.allows_deletion(Path::new("data/temp/cache.dat"), false));
    assert!(!set2.allows_deletion(Path::new("data/important.dat"), false));
}

/// Verifies exception pattern works for protect/risk.
#[test]
fn protect_exception_pattern() {
    let rules = [
        FilterRule::risk("archive/temp/"), // Exception first
        FilterRule::protect("archive/"),   // General protection second
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // temp can be deleted (risk matches first)
    assert!(set.allows_deletion(Path::new("archive/temp/file.dat"), false));
    // other archive contents protected
    assert!(!set.allows_deletion(Path::new("archive/important.dat"), false));
}

// =============================================================================
// Include/Exclude Separate from Protect/Risk
// =============================================================================

/// Verifies include/exclude and protect/risk are evaluated independently.
#[test]
fn include_exclude_independent_from_protect_risk() {
    let rules = [FilterRule::exclude("*.tmp"), FilterRule::protect("*.tmp")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: excluded
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    // Deletion: protected (separate evaluation)
    assert!(!set.allows_deletion(Path::new("scratch.tmp"), false));
}

/// Order within each category matters independently.
/// Note: allows_deletion() requires transfer_allowed AND not protected.
/// So exclude rules block deletion even if risk rule would allow.
#[test]
fn order_matters_per_category() {
    let rules = [
        // Include/exclude: include *.tmp first
        FilterRule::include("*.tmp"),
        FilterRule::exclude("keep.tmp"),
        // Protect/risk: protect first
        FilterRule::protect("*.tmp"),
        FilterRule::risk("keep.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: include matches first, all .tmp allowed
    assert!(set.allows(Path::new("keep.tmp"), false));
    assert!(set.allows(Path::new("other.tmp"), false));
    // Deletion: include allows transfer, protect matches first for protection
    assert!(!set.allows_deletion(Path::new("keep.tmp"), false));
    assert!(!set.allows_deletion(Path::new("other.tmp"), false));
}

// =============================================================================
// Wildcard Pattern Order
// =============================================================================

/// Verifies order matters for overlapping wildcard patterns.
#[test]
fn overlapping_wildcards_order() {
    // ** before specific: ** matches everything first
    let rules_general_first = [
        FilterRule::exclude("**/*.log"),
        FilterRule::include("error.log"),
    ];
    let set1 = FilterSet::from_rules(rules_general_first).unwrap();
    // error.log matches **/*.log first (at root, ** matches empty)
    assert!(!set1.allows(Path::new("error.log"), false));

    // Specific before **: specific wins for exact matches
    let rules_specific_first = [
        FilterRule::include("error.log"),
        FilterRule::exclude("**/*.log"),
    ];
    let set2 = FilterSet::from_rules(rules_specific_first).unwrap();
    assert!(set2.allows(Path::new("error.log"), false));
}

/// Verifies ** pattern matches at any depth.
#[test]
fn double_star_pattern_order() {
    let rules = [
        FilterRule::include("src/**/test/**"),
        FilterRule::exclude("**/test/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // test dirs under src included
    assert!(set.allows(Path::new("src/lib/test/data.txt"), false));
    // test dirs elsewhere excluded
    assert!(!set.allows(Path::new("vendor/test/data.txt"), false));
}

// =============================================================================
// Directory Pattern Order
// =============================================================================

/// Verifies directory-only patterns follow order semantics.
#[test]
fn directory_only_pattern_order() {
    let rules = [
        FilterRule::include("build/output/"),
        FilterRule::exclude("build/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // build/output/ included (first rule matches for directories)
    assert!(set.allows(Path::new("build/output"), true));
    // build/output/file.bin also included (descendant matching)
    assert!(set.allows(Path::new("build/output/file.bin"), false));
    // build/ excluded (second rule)
    assert!(!set.allows(Path::new("build"), true));
    // build/other.bin excluded (descendant of excluded dir)
    assert!(!set.allows(Path::new("build/other.bin"), false));
}

/// Verifies file patterns don't match directory-only rules.
#[test]
fn directory_only_vs_file_order() {
    let rules = [FilterRule::exclude("cache/"), FilterRule::include("cache")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory named cache: directory-only exclude matches
    assert!(!set.allows(Path::new("cache"), true));
    // File named cache: directory-only doesn't match, include does
    assert!(set.allows(Path::new("cache"), false));
}

// =============================================================================
// Anchored Pattern Order
// =============================================================================

/// Verifies anchored patterns follow order with unanchored.
#[test]
fn anchored_and_unanchored_order() {
    let rules = [
        FilterRule::include("/build/release/**"),
        FilterRule::exclude("build/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // /build/release/** included at root
    assert!(set.allows(Path::new("build/release/app.bin"), false));
    // /build/ at root excluded (but release is still included above)
    assert!(!set.allows(Path::new("build/debug/app.bin"), false));
    // build/ elsewhere excluded too (unanchored)
    assert!(!set.allows(Path::new("sub/build/file"), false));
}

/// Verifies anchored patterns only match at root.
#[test]
fn anchored_root_only_order() {
    let rules = [FilterRule::exclude("/temp"), FilterRule::include("temp")];
    let set = FilterSet::from_rules(rules).unwrap();

    // temp at root: anchored exclude matches first
    assert!(!set.allows(Path::new("temp"), false));
    // temp elsewhere: anchored doesn't match, unanchored include matches
    assert!(set.allows(Path::new("dir/temp"), false));
}

// =============================================================================
// Sender/Receiver Side Order
// =============================================================================

/// Verifies sender-only rules follow order for allows().
#[test]
fn sender_only_order() {
    let rules = [
        FilterRule::hide("*.secret"),         // Sender-only exclude
        FilterRule::show("important.secret"), // Sender-only include
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // important.secret: hide matches first on sender
    assert!(!set.allows(Path::new("important.secret"), false));
    assert!(!set.allows(Path::new("other.secret"), false));
}

/// Verifies receiver-only rules follow order for allows_deletion().
#[test]
fn receiver_only_order() {
    let rules = [
        FilterRule::exclude("*.tmp").with_sides(false, true), // Receiver only
        FilterRule::include("keep.tmp").with_sides(false, true), // Receiver only
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer (sender): receiver-only rules don't apply
    assert!(set.allows(Path::new("keep.tmp"), false));
    assert!(set.allows(Path::new("other.tmp"), false));

    // Deletion (receiver): exclude matches first
    assert!(!set.allows_deletion(Path::new("keep.tmp"), false));
    assert!(!set.allows_deletion(Path::new("other.tmp"), false));
}

/// Verifies mixed side rules follow order per context.
#[test]
fn mixed_sides_order() {
    let rules = [
        FilterRule::exclude("*.log").with_sides(true, false), // Sender only
        FilterRule::exclude("*.log").with_sides(false, true), // Receiver only
        FilterRule::include("*.log"),                         // Both
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender: first rule matches
    assert!(!set.allows(Path::new("app.log"), false));
    // Receiver: second rule matches
    assert!(!set.allows_deletion(Path::new("app.log"), false));
}

// =============================================================================
// Perishable Rules Order
// =============================================================================

/// Verifies perishable rules are skipped in deletion context.
#[test]
fn perishable_order_in_deletion() {
    let rules = [
        FilterRule::exclude("*.tmp").with_perishable(true),
        FilterRule::include("*.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: perishable exclude matches first
    assert!(!set.allows(Path::new("scratch.tmp"), false));

    // Deletion: perishable rule skipped, include matches
    assert!(set.allows_deletion(Path::new("scratch.tmp"), false));
}

/// Verifies non-perishable followed by perishable.
#[test]
fn perishable_after_non_perishable() {
    let rules = [
        FilterRule::exclude("important.tmp"), // Non-perishable
        FilterRule::exclude("*.tmp").with_perishable(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // important.tmp: excluded by non-perishable rule
    assert!(!set.allows(Path::new("important.tmp"), false));
    assert!(!set.allows_deletion(Path::new("important.tmp"), false));

    // other.tmp: excluded by perishable rule for transfer
    assert!(!set.allows(Path::new("other.tmp"), false));
    // For deletion, perishable skipped, include matches
    assert!(set.allows_deletion(Path::new("other.tmp"), false));
}

// =============================================================================
// Clear Rule Order Effects
// =============================================================================

/// Verifies clear removes all previous rules.
#[test]
fn clear_resets_evaluation_chain() {
    let rules = [
        FilterRule::exclude("*.log"),
        FilterRule::exclude("*.tmp"),
        FilterRule::clear(),
        FilterRule::exclude("*.bak"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Rules before clear are gone
    assert!(set.allows(Path::new("app.log"), false));
    assert!(set.allows(Path::new("scratch.tmp"), false));
    // Rule after clear is active
    assert!(!set.allows(Path::new("backup.bak"), false));
}

/// Verifies clear establishes new first-match-wins chain.
#[test]
fn clear_new_chain_order() {
    let rules = [
        FilterRule::include("*"), // Would include everything
        FilterRule::clear(),
        FilterRule::include("keep.txt"), // First in new chain
        FilterRule::exclude("*.txt"),    // Second in new chain
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Old include("*") cleared
    // New chain: include keep.txt first
    assert!(set.allows(Path::new("keep.txt"), false));
    // exclude *.txt second
    assert!(!set.allows(Path::new("other.txt"), false));
}

/// Multiple clears create multiple fresh chains.
#[test]
fn multiple_clears_order() {
    let rules = [
        FilterRule::exclude("a"),
        FilterRule::clear(),
        FilterRule::exclude("b"),
        FilterRule::clear(),
        FilterRule::exclude("c"),
        FilterRule::include("c"), // After the exclude
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All before last clear are gone
    assert!(set.allows(Path::new("a"), false));
    assert!(set.allows(Path::new("b"), false));
    // Last chain: exclude("c") first, include("c") second
    assert!(!set.allows(Path::new("c"), false));
}

// =============================================================================
// Complex Order Scenarios
// =============================================================================

/// Real-world scenario: backup with exceptions.
#[test]
fn backup_scenario_order() {
    let rules = [
        // Exceptions for important logs (first)
        FilterRule::include("error.log"),
        FilterRule::include("audit.log"),
        // Exclude temporary and log files (second)
        FilterRule::exclude("*.log"),
        FilterRule::exclude("*.tmp"),
        FilterRule::exclude("*.swp"),
        // Protect important files from deletion
        FilterRule::protect("*.dat"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Important logs included
    assert!(set.allows(Path::new("error.log"), false));
    assert!(set.allows(Path::new("audit.log"), false));
    // Other logs excluded
    assert!(!set.allows(Path::new("debug.log"), false));
    // Temp files excluded
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    // Data files allowed and protected
    assert!(set.allows(Path::new("data.dat"), false));
    assert!(!set.allows_deletion(Path::new("data.dat"), false));
}

/// Real-world scenario: sync source code only.
#[test]
fn source_sync_scenario_order() {
    let rules = [
        // First: explicitly include source extensions
        FilterRule::include("*.rs"),
        FilterRule::include("*.toml"),
        FilterRule::include("*.md"),
        // Second: exclude everything else at root and in subdirs
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Source files included (rules 1-3 match first)
    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("Cargo.toml"), false));
    assert!(set.allows(Path::new("README.md"), false));
    // Nested source files also included (unanchored patterns)
    assert!(set.allows(Path::new("src/lib.rs"), false));
    // Other files excluded (rule 4 matches)
    assert!(!set.allows(Path::new("image.png"), false));
    assert!(!set.allows(Path::new("data.bin"), false));
}

/// Complex nested directory scenario.
#[test]
fn nested_directory_order() {
    let rules = [
        // Include specific paths first
        FilterRule::include("src/"),
        FilterRule::include("src/**"),
        FilterRule::include("tests/fixtures/"),
        FilterRule::include("tests/fixtures/**"),
        // Exclude test directories generally
        FilterRule::exclude("tests/"),
        // Exclude vendor
        FilterRule::exclude("vendor/"),
        // Include everything else
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // src included
    assert!(set.allows(Path::new("src/main.rs"), false));
    // test fixtures included
    assert!(set.allows(Path::new("tests/fixtures/data.json"), false));
    // other tests excluded
    assert!(!set.allows(Path::new("tests/unit.rs"), false));
    // vendor excluded
    assert!(!set.allows(Path::new("vendor/lib.rs"), false));
    // other files included
    assert!(set.allows(Path::new("build.rs"), false));
}

// =============================================================================
// Edge Cases
// =============================================================================

/// Same pattern with different actions - first wins.
#[test]
fn same_pattern_different_actions() {
    let rules = [
        FilterRule::exclude("file.txt"),
        FilterRule::include("file.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // exclude comes first
    assert!(!set.allows(Path::new("file.txt"), false));
}

/// Many rules with single match.
#[test]
fn many_rules_single_match() {
    let rules: Vec<_> = (0..100)
        .map(|i| FilterRule::exclude(format!("file{i}.txt")))
        .collect();
    let set = FilterSet::from_rules(rules).unwrap();

    // Only file50.txt should be excluded
    assert!(!set.allows(Path::new("file50.txt"), false));
    // Others not in list allowed
    assert!(set.allows(Path::new("file100.txt"), false));
    assert!(set.allows(Path::new("other.txt"), false));
}

/// Verifies correct behavior when pattern could match multiple rules.
#[test]
fn multiple_potential_matches() {
    let rules = [
        FilterRule::exclude("*.txt"),
        FilterRule::exclude("readme.*"),
        FilterRule::exclude("readme.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // readme.txt matches all three, but first wins
    assert!(!set.allows(Path::new("readme.txt"), false));
    // Each rule can also match independently
    assert!(!set.allows(Path::new("other.txt"), false));
    assert!(!set.allows(Path::new("readme.md"), false));
}

/// Trailing slash handling in order.
#[test]
fn trailing_slash_order() {
    let rules = [
        FilterRule::include("data/"), // Directory only
        FilterRule::exclude("data"),  // File or directory
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory: include matches first
    assert!(set.allows(Path::new("data"), true));
    // File: include doesn't match (directory only), exclude matches
    assert!(!set.allows(Path::new("data"), false));
}

/// Leading slash handling in order.
#[test]
fn leading_slash_order() {
    let rules = [
        FilterRule::include("/config"), // Anchored to root
        FilterRule::exclude("config"),  // Anywhere
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // At root: anchored include matches first
    assert!(set.allows(Path::new("config"), false));
    // Elsewhere: anchored doesn't match, unanchored exclude does
    assert!(!set.allows(Path::new("dir/config"), false));
}
