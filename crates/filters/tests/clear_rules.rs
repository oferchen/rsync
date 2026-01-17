//! Integration tests for clear filter rules.
//!
//! These tests verify the behavior of the clear rule (`!`) which removes
//! previously defined filter rules. Clear rules can target specific sides
//! (sender/receiver) and affect both include/exclude and protect/risk rules.
//!
//! Reference: rsync 3.4.1 exclude.c lines 1393-1401 for clear rule handling.

use filters::{FilterAction, FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Basic Clear Rule Tests
// ============================================================================

/// Verifies clear rule removes all previous rules.
#[test]
fn clear_removes_all_previous_rules() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::include("important/"),
        FilterRule::clear(),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All rules cleared, empty set allows everything
    assert!(set.is_empty());
    assert!(set.allows(Path::new("file.tmp"), false));
    assert!(set.allows(Path::new("anything"), false));
}

/// Verifies clear rule removes protect rules.
#[test]
fn clear_removes_protect_rules() {
    let rules = [FilterRule::protect("critical/"), FilterRule::clear()];
    let set = FilterSet::from_rules(rules).unwrap();

    // Protection is gone
    assert!(set.allows_deletion(Path::new("critical/data.dat"), false));
}

/// Verifies clear rule properties.
#[test]
fn clear_rule_properties() {
    let rule = FilterRule::clear();

    assert_eq!(rule.action(), FilterAction::Clear);
    assert!(rule.pattern().is_empty());
    assert!(rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

// ============================================================================
// Rules After Clear Tests
// ============================================================================

/// Verifies rules added after clear work normally.
#[test]
fn rules_after_clear_work() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::clear(),
        FilterRule::exclude("*.bak"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Old rule is cleared
    assert!(set.allows(Path::new("scratch.tmp"), false));

    // New rule works
    assert!(!set.allows(Path::new("backup.bak"), false));
}

/// Verifies multiple clear rules work.
#[test]
fn multiple_clear_rules() {
    let rules = [
        FilterRule::exclude("*.a"),
        FilterRule::clear(),
        FilterRule::exclude("*.b"),
        FilterRule::clear(),
        FilterRule::exclude("*.c"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Only the last rule after the last clear is active
    assert!(set.allows(Path::new("file.a"), false));
    assert!(set.allows(Path::new("file.b"), false));
    assert!(!set.allows(Path::new("file.c"), false));
}

/// Verifies clear followed by no rules results in empty set.
#[test]
fn clear_at_end() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::include("important/"),
        FilterRule::protect("critical/"),
        FilterRule::clear(),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.is_empty());
}

// ============================================================================
// Side-Specific Clear Tests
// ============================================================================

/// Verifies clear with sender-only flag only clears sender rules.
#[test]
fn clear_sender_only() {
    let rules = [
        FilterRule::exclude("sender.txt").with_sides(true, false),
        FilterRule::exclude("receiver.txt").with_sides(false, true),
        FilterRule::exclude("both.txt"),
        FilterRule::clear().with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender-only rule is cleared
    assert!(set.allows(Path::new("sender.txt"), false));

    // Receiver-only rule remains
    assert!(!set.allows_deletion(Path::new("receiver.txt"), false));

    // Both-sides rule: sender side cleared, receiver side remains
    assert!(set.allows(Path::new("both.txt"), false)); // Sender cleared
    assert!(!set.allows_deletion(Path::new("both.txt"), false)); // Receiver remains
}

/// Verifies clear with receiver-only flag only clears receiver rules.
#[test]
fn clear_receiver_only() {
    let rules = [
        FilterRule::exclude("sender.txt").with_sides(true, false),
        FilterRule::exclude("receiver.txt").with_sides(false, true),
        FilterRule::exclude("both.txt"),
        FilterRule::clear().with_sides(false, true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender-only rule remains
    assert!(!set.allows(Path::new("sender.txt"), false));

    // Receiver-only rule is cleared
    assert!(set.allows_deletion(Path::new("receiver.txt"), false));

    // Both-sides rule: receiver side cleared, sender side remains
    assert!(!set.allows(Path::new("both.txt"), false)); // Sender remains
    assert!(set.allows_deletion(Path::new("both.txt"), false)); // Receiver cleared
}

/// Verifies clear with no sides has no effect.
#[test]
fn clear_no_sides_has_no_effect() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::clear().with_sides(false, false),
        FilterRule::exclude("*.bak"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Both rules are still active
    assert!(!set.allows(Path::new("file.tmp"), false));
    assert!(!set.allows(Path::new("file.bak"), false));
}

// ============================================================================
// Clear with Mixed Rule Types
// ============================================================================

/// Verifies clear affects both include/exclude and protect/risk.
#[test]
fn clear_affects_all_rule_types() {
    let rules = [
        FilterRule::include("keep.txt"),
        FilterRule::exclude("remove.txt"),
        FilterRule::protect("safe.txt"),
        FilterRule::risk("danger.txt"),
        FilterRule::clear(),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All rules cleared
    assert!(set.is_empty());
}

/// Verifies protect rules cleared before new include/exclude.
#[test]
fn clear_before_new_protect() {
    let rules = [
        FilterRule::protect("old_protected.txt"),
        FilterRule::clear(),
        FilterRule::protect("new_protected.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Old protection cleared
    assert!(set.allows_deletion(Path::new("old_protected.txt"), false));

    // New protection active
    assert!(!set.allows_deletion(Path::new("new_protected.txt"), false));
}

/// Verifies complex clear scenario with mixed rules.
#[test]
fn complex_clear_scenario() {
    let rules = [
        FilterRule::exclude("*.log"),
        FilterRule::protect("important.log"),
        FilterRule::clear(),
        FilterRule::exclude("*.tmp"),
        FilterRule::protect("important.tmp"),
        FilterRule::risk("important.tmp"), // Undoes protection
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Old log rule cleared
    assert!(set.allows(Path::new("debug.log"), false));

    // New tmp rule active
    assert!(!set.allows(Path::new("scratch.tmp"), false));

    // important.tmp is excluded and not protected (risk undid protect)
    // However, allows_deletion requires transfer_allowed to be true,
    // so excluded files are never deletable via allows_deletion
    assert!(!set.allows(Path::new("important.tmp"), false));
    assert!(!set.allows_deletion(Path::new("important.tmp"), false));

    // But if we check delete-excluded behavior, the file can be removed
    // since it's excluded and not protected
    assert!(set.allows_deletion_when_excluded_removed(Path::new("important.tmp"), false));
}

// ============================================================================
// Clear with Show/Hide Rules
// ============================================================================

/// Verifies clear removes show rules.
#[test]
fn clear_removes_show_rules() {
    let rules = [FilterRule::show("logs/**"), FilterRule::clear()];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.is_empty());
}

/// Verifies clear removes hide rules.
#[test]
fn clear_removes_hide_rules() {
    let rules = [FilterRule::hide("secret/**"), FilterRule::clear()];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.is_empty());
}

/// Verifies sender-only clear removes show/hide.
#[test]
fn sender_clear_removes_show_hide() {
    let rules = [
        FilterRule::show("visible/**"),
        FilterRule::hide("hidden/**"),
        FilterRule::clear().with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Show/hide are sender-only, so sender clear removes them
    assert!(set.is_empty());
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies clear on empty filter set.
#[test]
fn clear_empty_set() {
    let rules = [FilterRule::clear()];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.is_empty());
}

/// Verifies multiple consecutive clears.
#[test]
fn consecutive_clears() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::clear(),
        FilterRule::clear(),
        FilterRule::clear(),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.is_empty());
}

/// Verifies clear preserves rules added after it.
#[test]
fn clear_preserves_subsequent_rules() {
    let rules = [
        FilterRule::exclude("before1.txt"),
        FilterRule::exclude("before2.txt"),
        FilterRule::clear(),
        FilterRule::exclude("after1.txt"),
        FilterRule::exclude("after2.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Before rules cleared
    assert!(set.allows(Path::new("before1.txt"), false));
    assert!(set.allows(Path::new("before2.txt"), false));

    // After rules active
    assert!(!set.allows(Path::new("after1.txt"), false));
    assert!(!set.allows(Path::new("after2.txt"), false));
}

/// Verifies clear doesn't affect rules it can't see (wrong side).
#[test]
fn clear_respects_side_boundaries() {
    let rules = [
        FilterRule::exclude("receiver.txt").with_sides(false, true),
        FilterRule::clear().with_sides(true, false), // Only clears sender
        FilterRule::exclude("sender.txt").with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Receiver rule untouched
    assert!(!set.allows_deletion(Path::new("receiver.txt"), false));

    // Sender rule added after clear
    assert!(!set.allows(Path::new("sender.txt"), false));
}

/// Verifies clear interaction with rules that apply to both sides.
#[test]
fn clear_partial_removes_from_both_sides_rule() {
    // A rule that applies to both sides has its sender component cleared
    // but receiver component remains
    let rules = [
        FilterRule::exclude("both.txt"),             // Applies to both sides
        FilterRule::clear().with_sides(true, false), // Clear sender only
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender side cleared: allows transfer
    assert!(set.allows(Path::new("both.txt"), false));

    // Receiver side remains: blocks deletion
    assert!(!set.allows_deletion(Path::new("both.txt"), false));
}

/// Verifies that clearing one side leaves the other side's rules intact.
#[test]
fn clear_one_side_preserves_other() {
    let rules = [
        // Rule 1: sender-only
        FilterRule::exclude("a.txt").with_sides(true, false),
        // Rule 2: receiver-only
        FilterRule::exclude("b.txt").with_sides(false, true),
        // Rule 3: both sides
        FilterRule::exclude("c.txt"),
        // Clear sender
        FilterRule::clear().with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // a.txt: sender rule cleared
    assert!(set.allows(Path::new("a.txt"), false));
    assert!(set.allows_deletion(Path::new("a.txt"), false));

    // b.txt: receiver rule still active
    assert!(set.allows(Path::new("b.txt"), false)); // No sender rule
    assert!(!set.allows_deletion(Path::new("b.txt"), false)); // Receiver rule blocks

    // c.txt: sender cleared, receiver remains
    assert!(set.allows(Path::new("c.txt"), false)); // Sender cleared
    assert!(!set.allows_deletion(Path::new("c.txt"), false)); // Receiver blocks
}
