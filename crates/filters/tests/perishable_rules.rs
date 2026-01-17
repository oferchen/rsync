//! Integration tests for perishable filter rules.
//!
//! These tests verify the behavior of perishable rules (`p` modifier) which
//! are ignored during deletion checks. Perishable rules are useful for
//! excluding files that should be skipped during transfer but not protected
//! from deletion during `--delete` operations.
//!
//! Reference: rsync 3.4.1 exclude.c lines 1044-1045 for perishable handling.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Basic Perishable Rule Tests
// ============================================================================

/// Verifies perishable exclude is ignored during deletion checks.
///
/// From rsync man page: "The p is for perishable, which means that this
/// rule does not apply during deletion."
#[test]
fn perishable_exclude_ignored_during_deletion() {
    let rule = FilterRule::exclude("*.tmp").with_perishable(true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Transfer is excluded (perishable still applies to transfer)
    assert!(!set.allows(Path::new("scratch.tmp"), false));

    // Deletion is allowed (perishable is ignored)
    assert!(set.allows_deletion(Path::new("scratch.tmp"), false));
}

/// Verifies non-perishable rules work normally.
#[test]
fn non_perishable_exclude_applies_to_deletion() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();

    // Transfer is excluded
    assert!(!set.allows(Path::new("scratch.tmp"), false));

    // Deletion is also blocked (non-perishable)
    assert!(!set.allows_deletion(Path::new("scratch.tmp"), false));
}

/// Verifies perishable flag is tracked correctly.
#[test]
fn perishable_flag_tracked() {
    let perishable = FilterRule::exclude("*.tmp").with_perishable(true);
    let non_perishable = FilterRule::exclude("*.bak");

    assert!(perishable.is_perishable());
    assert!(!non_perishable.is_perishable());
}

// ============================================================================
// Perishable Include Rules
// ============================================================================

/// Verifies perishable include is ignored during deletion.
#[test]
fn perishable_include_ignored_during_deletion() {
    let rules = [
        FilterRule::exclude("*"),
        FilterRule::include("keep/**").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer includes keep/** (perishable include applies)
    assert!(set.allows(Path::new("keep/file.txt"), false));

    // Deletion: perishable include is ignored, so exclude * applies
    // This means the file can be deleted
    assert!(!set.allows_deletion(Path::new("keep/file.txt"), false));
}

/// Verifies perishable and non-perishable includes interact correctly.
#[test]
fn perishable_and_non_perishable_includes() {
    let rules = [
        FilterRule::exclude("*"),
        FilterRule::include("permanent/**"), // Non-perishable
        FilterRule::include("temporary/**").with_perishable(true), // Perishable
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Both are included in transfer
    assert!(set.allows(Path::new("permanent/file.txt"), false));
    assert!(set.allows(Path::new("temporary/file.txt"), false));

    // For deletion: perishable include for temporary is skipped
    // permanent/** still matches (non-perishable), so transfer_allowed=true, deletable
    assert!(set.allows_deletion(Path::new("permanent/file.txt"), false));

    // For temporary: perishable include is skipped, exclude * matches, transfer_allowed=false
    assert!(!set.allows_deletion(Path::new("temporary/file.txt"), false));
}

// ============================================================================
// Perishable with Delete-Excluded
// ============================================================================

/// Verifies perishable affects delete-excluded behavior.
#[test]
fn perishable_delete_excluded() {
    let rule = FilterRule::exclude("*.tmp").with_perishable(true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // For allows_deletion_when_excluded_removed, perishable is included
    // (it checks with include_perishable=true)
    assert!(set.allows_deletion_when_excluded_removed(Path::new("scratch.tmp"), false));
}

/// Verifies non-perishable exclude with delete-excluded.
#[test]
fn non_perishable_delete_excluded() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();

    // For delete-excluded, the excluded file can be deleted
    assert!(set.allows_deletion_when_excluded_removed(Path::new("scratch.tmp"), false));
}

// ============================================================================
// Perishable with Rule Ordering
// ============================================================================

/// Verifies perishable exclude after non-perishable include.
#[test]
fn perishable_exclude_after_include() {
    let rules = [
        FilterRule::include("*.txt"),
        FilterRule::exclude("temp_*.txt").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Regular txt files included in transfer
    assert!(set.allows(Path::new("document.txt"), false));
    // For deletion: perishable exclude is skipped, include *.txt matches, deletable
    assert!(set.allows_deletion(Path::new("document.txt"), false));

    // Temp txt files excluded from transfer (perishable applies on sender)
    assert!(!set.allows(Path::new("temp_scratch.txt"), false));
    // For deletion: perishable exclude skipped, include *.txt matches, deletable
    assert!(set.allows_deletion(Path::new("temp_scratch.txt"), false));
}

/// Verifies non-perishable exclude after perishable include.
#[test]
fn non_perishable_exclude_after_perishable_include() {
    let rules = [
        FilterRule::exclude("*"),
        FilterRule::include("*.txt").with_perishable(true),
        FilterRule::exclude("secret.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Regular txt files included in transfer
    assert!(set.allows(Path::new("document.txt"), false));

    // But deletion: perishable include is ignored, so exclude * applies
    // However, non-perishable exclude also applies
    assert!(!set.allows_deletion(Path::new("document.txt"), false));

    // Secret is excluded from transfer
    assert!(!set.allows(Path::new("secret.txt"), false));
}

// ============================================================================
// Perishable with Directory Patterns
// ============================================================================

/// Verifies perishable with directory pattern.
#[test]
fn perishable_directory_pattern() {
    let rule = FilterRule::exclude("cache/").with_perishable(true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Directory excluded from transfer
    assert!(!set.allows(Path::new("cache"), true));
    assert!(!set.allows(Path::new("cache/file.dat"), false));

    // But deletable
    assert!(set.allows_deletion(Path::new("cache"), true));
    assert!(set.allows_deletion(Path::new("cache/file.dat"), false));
}

/// Verifies perishable with nested directory patterns.
#[test]
fn perishable_nested_directory() {
    let rules = [
        FilterRule::exclude("build/").with_perishable(true),
        FilterRule::exclude("build/release/"), // Non-perishable
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All excluded from transfer
    assert!(!set.allows(Path::new("build/debug/file"), false));
    assert!(!set.allows(Path::new("build/release/binary"), false));

    // Debug is deletable (perishable parent), release is not (non-perishable)
    assert!(set.allows_deletion(Path::new("build/debug/file"), false));
    assert!(!set.allows_deletion(Path::new("build/release/binary"), false));
}

// ============================================================================
// Perishable with Protect/Risk
// ============================================================================

/// Verifies perishable exclude with protect.
#[test]
fn perishable_with_protect() {
    let rules = [
        FilterRule::exclude("*.tmp").with_perishable(true),
        FilterRule::protect("important.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Excluded from transfer (perishable exclude applies)
    assert!(!set.allows(Path::new("important.tmp"), false));

    // Protected from deletion (protect applies regardless of perishable)
    assert!(!set.allows_deletion(Path::new("important.tmp"), false));

    // Other tmp files excluded and deletable
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    assert!(set.allows_deletion(Path::new("scratch.tmp"), false));
}

/// Verifies perishable interacts correctly with risk.
#[test]
fn perishable_with_risk() {
    let rules = [
        FilterRule::exclude("archive/").with_perishable(true),
        FilterRule::protect("archive/"),
        FilterRule::risk("archive/old/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All excluded from transfer
    assert!(!set.allows(Path::new("archive/current/file"), false));
    assert!(!set.allows(Path::new("archive/old/file"), false));

    // Current is protected, old is at risk
    assert!(!set.allows_deletion(Path::new("archive/current/file"), false));
    assert!(set.allows_deletion(Path::new("archive/old/file"), false));
}

// ============================================================================
// Perishable with Side Modifiers
// ============================================================================

/// Verifies perishable with sender-only rule.
#[test]
fn perishable_sender_only() {
    let rule = FilterRule::exclude("*.tmp")
        .with_perishable(true)
        .with_sides(true, false);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Excluded from transfer (sender side)
    assert!(!set.allows(Path::new("file.tmp"), false));

    // Deletable (no receiver rule, and perishable anyway)
    assert!(set.allows_deletion(Path::new("file.tmp"), false));
}

/// Verifies perishable with receiver-only rule.
#[test]
fn perishable_receiver_only() {
    let rule = FilterRule::exclude("*.tmp")
        .with_perishable(true)
        .with_sides(false, true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Transfer allowed (no sender rule)
    assert!(set.allows(Path::new("file.tmp"), false));

    // Deletion: perishable is ignored, so file is deletable
    assert!(set.allows_deletion(Path::new("file.tmp"), false));
}

// ============================================================================
// Perishable with Clear
// ============================================================================

/// Verifies clear removes perishable rules.
#[test]
fn clear_removes_perishable() {
    let rules = [
        FilterRule::exclude("*.tmp").with_perishable(true),
        FilterRule::clear(),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Rule is cleared
    assert!(set.allows(Path::new("file.tmp"), false));
}

/// Verifies rules after clear can be perishable.
#[test]
fn perishable_after_clear() {
    let rules = [
        FilterRule::exclude("*.old"),
        FilterRule::clear(),
        FilterRule::exclude("*.tmp").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Old rule cleared
    assert!(set.allows(Path::new("file.old"), false));

    // New perishable rule works
    assert!(!set.allows(Path::new("file.tmp"), false));
    assert!(set.allows_deletion(Path::new("file.tmp"), false));
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies perishable with empty filter set.
#[test]
fn only_perishable_rule() {
    let rule = FilterRule::exclude("*.tmp").with_perishable(true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Non-matching files allowed
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(set.allows_deletion(Path::new("file.txt"), false));

    // Matching files excluded from transfer but deletable
    assert!(!set.allows(Path::new("file.tmp"), false));
    assert!(set.allows_deletion(Path::new("file.tmp"), false));
}

/// Verifies all rules can be perishable.
#[test]
fn all_perishable_rules() {
    let rules = [
        FilterRule::exclude("*.tmp").with_perishable(true),
        FilterRule::include("important.tmp").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: important.tmp is included
    assert!(set.allows(Path::new("important.tmp"), false));
    assert!(!set.allows(Path::new("scratch.tmp"), false));

    // Deletion: all perishable rules ignored, defaults to allow
    assert!(set.allows_deletion(Path::new("important.tmp"), false));
    assert!(set.allows_deletion(Path::new("scratch.tmp"), false));
}

/// Verifies with_perishable is a builder method.
#[test]
fn perishable_builder_pattern() {
    let rule = FilterRule::exclude("*.tmp")
        .with_perishable(true)
        .with_sender(true)
        .with_receiver(false);

    assert!(rule.is_perishable());
    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

/// Verifies perishable can be toggled.
#[test]
fn perishable_toggle() {
    let rule = FilterRule::exclude("*.tmp")
        .with_perishable(true)
        .with_perishable(false);

    assert!(!rule.is_perishable());
}

/// Verifies complex scenario with multiple perishable rules.
#[test]
fn complex_perishable_scenario() {
    let rules = [
        FilterRule::exclude("*"),
        FilterRule::include("src/**"),
        FilterRule::include("cache/**").with_perishable(true),
        FilterRule::exclude("src/**/*.tmp").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // src files included in transfer
    assert!(set.allows(Path::new("src/main.rs"), false));
    // For deletion: perishable exclude skipped, include src/** matches, deletable
    assert!(set.allows_deletion(Path::new("src/main.rs"), false));

    // src tmp files excluded from transfer (perishable applies on sender)
    assert!(!set.allows(Path::new("src/scratch.tmp"), false));
    // For deletion: perishable exclude skipped, include src/** matches, deletable
    assert!(set.allows_deletion(Path::new("src/scratch.tmp"), false));

    // cache files included in transfer
    assert!(set.allows(Path::new("cache/data.bin"), false));

    // cache deletion: perishable include ignored, exclude * applies
    assert!(!set.allows_deletion(Path::new("cache/data.bin"), false));
}
