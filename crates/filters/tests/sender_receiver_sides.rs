//! Integration tests for sender/receiver side filter rules.
//!
//! These tests verify the behavior of filter rules that apply to specific
//! sides of the rsync transfer: sender-side (show/hide) and receiver-side
//! (protect/risk), as well as rules modified with `with_sender`/`with_receiver`.
//!
//! Reference: rsync 3.4.1 exclude.c lines 1194-1207 for side modifiers.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Show Rule Tests (Sender-Side Include)
// ============================================================================

/// Verifies show rules are sender-only includes.
///
/// From rsync man page: "S is a show prefix, which makes the rule a sender-side
/// include rule. Sender-side rules can affect which files are shown in the
/// transfer list without affecting the receiver."
#[test]
fn show_rule_applies_to_sender_only() {
    let rule = FilterRule::show("logs/**");

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

/// Verifies show rule behavior in filter set.
#[test]
fn show_rule_allows_transfer_but_not_delete_block() {
    let set = FilterSet::from_rules([FilterRule::show("visible.txt")]).unwrap();

    // Shows file on sender (allows transfer)
    assert!(set.allows(Path::new("visible.txt"), false));

    // But receiver can still delete (no receiver-side effect)
    assert!(set.allows_deletion(Path::new("visible.txt"), false));
}

/// Verifies show rules with wildcard patterns.
#[test]
fn show_rule_wildcard() {
    let set = FilterSet::from_rules([FilterRule::show("*.log")]).unwrap();

    assert!(set.allows(Path::new("app.log"), false));
    assert!(set.allows(Path::new("error.log"), false));
    assert!(set.allows(Path::new("system.log"), false));
}

// ============================================================================
// Hide Rule Tests (Sender-Side Exclude)
// ============================================================================

/// Verifies hide rules are sender-only excludes.
///
/// From rsync man page: "H is a hide prefix, which makes the rule a sender-side
/// exclude rule. Sender-side rules can affect which files are hidden from the
/// transfer list without affecting the receiver."
#[test]
fn hide_rule_applies_to_sender_only() {
    let rule = FilterRule::hide("*.bak");

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

/// Verifies hide rule behavior in filter set.
#[test]
fn hide_rule_blocks_transfer_but_not_delete() {
    let set = FilterSet::from_rules([FilterRule::hide("hidden.txt")]).unwrap();

    // Hides file on sender (blocks transfer)
    assert!(!set.allows(Path::new("hidden.txt"), false));

    // But receiver can still delete (no receiver-side effect)
    assert!(set.allows_deletion(Path::new("hidden.txt"), false));
}

/// Verifies hide rules with directory patterns.
#[test]
fn hide_rule_directory_pattern() {
    let set = FilterSet::from_rules([FilterRule::hide(".git/")]).unwrap();

    // Directory is hidden
    assert!(!set.allows(Path::new(".git"), true));

    // Contents are hidden
    assert!(!set.allows(Path::new(".git/config"), false));
    assert!(!set.allows(Path::new(".git/objects/pack/pack.idx"), false));

    // Other directories are visible
    assert!(set.allows(Path::new("src"), true));
}

// ============================================================================
// Include/Exclude with Side Modifiers
// ============================================================================

/// Verifies include with sender-only modifier.
#[test]
fn include_sender_only() {
    let rule = FilterRule::include("*.txt").with_sides(true, false);

    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

/// Verifies exclude with receiver-only modifier.
#[test]
fn exclude_receiver_only() {
    let rule = FilterRule::exclude("*.tmp").with_sides(false, true);

    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

/// Verifies sender-only exclude does not block receiver deletion.
#[test]
fn sender_only_exclude_allows_receiver_deletion() {
    let rules = [FilterRule::exclude("skip.txt").with_sides(true, false)];
    let set = FilterSet::from_rules(rules).unwrap();

    // Excluded from sender (blocked from transfer)
    assert!(!set.allows(Path::new("skip.txt"), false));

    // But receiver allows deletion (rule doesn't apply)
    assert!(set.allows_deletion(Path::new("skip.txt"), false));
}

/// Verifies receiver-only exclude does not hide from sender.
#[test]
fn receiver_only_exclude_allows_sender_transfer() {
    let rules = [FilterRule::exclude("keep.txt").with_sides(false, true)];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender allows transfer (rule doesn't apply)
    assert!(set.allows(Path::new("keep.txt"), false));

    // Receiver blocks deletion
    assert!(!set.allows_deletion(Path::new("keep.txt"), false));
}

// ============================================================================
// Mixed Side Rules Interactions
// ============================================================================

/// Verifies sender-only and receiver-only rules interact correctly.
#[test]
fn sender_and_receiver_rules_interaction() {
    let rules = [
        // Sender-only exclude: hide from transfer
        FilterRule::exclude("sender_hidden.txt").with_sides(true, false),
        // Receiver-only exclude: block deletion
        FilterRule::exclude("receiver_protected.txt").with_sides(false, true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // sender_hidden: excluded from transfer, but deletable
    assert!(!set.allows(Path::new("sender_hidden.txt"), false));
    assert!(set.allows_deletion(Path::new("sender_hidden.txt"), false));

    // receiver_protected: included in transfer, but not deletable
    assert!(set.allows(Path::new("receiver_protected.txt"), false));
    assert!(!set.allows_deletion(Path::new("receiver_protected.txt"), false));
}

/// Verifies rule ordering with different sides.
#[test]
fn rule_ordering_with_sides() {
    let rules = [
        // Exclude everything on receiver
        FilterRule::exclude("*").with_sides(false, true),
        // But include on sender
        FilterRule::include("*.txt").with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender allows .txt files
    assert!(set.allows(Path::new("readme.txt"), false));

    // Receiver blocks deletion (exclude * still applies)
    assert!(!set.allows_deletion(Path::new("readme.txt"), false));
}

/// Verifies sender-only include before sender-only exclude.
///
/// With first-match-wins, specific include must come before general exclude.
#[test]
fn sender_only_include_after_sender_only_exclude() {
    // Specific include first, then general exclude
    let rules = [
        FilterRule::include("important.log").with_sides(true, false),
        FilterRule::exclude("*.log").with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // General .log files excluded (exclude matches)
    assert!(!set.allows(Path::new("debug.log"), false));

    // Important log included (include matches first)
    assert!(set.allows(Path::new("important.log"), false));
}

// ============================================================================
// Show/Hide with Include/Exclude Combinations
// ============================================================================

/// Verifies show rule interacts with exclude rule.
///
/// With first-match-wins, show must come before exclude.
#[test]
fn show_with_exclude_interaction() {
    // Show specific first, then exclude all
    let rules = [
        FilterRule::show("important/**"), // Show important on sender first
        FilterRule::exclude("*"),         // Then exclude everything else
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Important files are shown (show matches first)
    assert!(set.allows(Path::new("important/file.txt"), false));

    // Other files are still excluded (exclude matches)
    assert!(!set.allows(Path::new("other/file.txt"), false));
}

/// Verifies hide rule interacts with include rule.
///
/// With first-match-wins, hide must come before include.
#[test]
fn hide_with_include_interaction() {
    // Hide specific first, then include all
    let rules = [
        FilterRule::hide("secret/**"), // Hide secret on sender first
        FilterRule::include("*"),      // Then include everything else
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Secret files are hidden (hide matches first)
    assert!(!set.allows(Path::new("secret/data.txt"), false));

    // Other files are still included (include matches)
    assert!(set.allows(Path::new("public/data.txt"), false));
}

// ============================================================================
// Clear Rule with Side Modifiers
// ============================================================================

/// Verifies clear rule clears only sender-side rules when specified.
#[test]
fn clear_sender_side_only() {
    let rules = [
        FilterRule::exclude("sender.txt").with_sides(true, false),
        FilterRule::exclude("receiver.txt").with_sides(false, true),
        FilterRule::clear().with_sides(true, false),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender rule is cleared
    assert!(set.allows(Path::new("sender.txt"), false));

    // Receiver rule remains
    assert!(!set.allows_deletion(Path::new("receiver.txt"), false));
}

/// Verifies clear rule clears only receiver-side rules when specified.
#[test]
fn clear_receiver_side_only() {
    let rules = [
        FilterRule::exclude("sender.txt").with_sides(true, false),
        FilterRule::exclude("receiver.txt").with_sides(false, true),
        FilterRule::clear().with_sides(false, true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Sender rule remains
    assert!(!set.allows(Path::new("sender.txt"), false));

    // Receiver rule is cleared
    assert!(set.allows_deletion(Path::new("receiver.txt"), false));
}

/// Verifies clear rule clears both sides when both are set.
#[test]
fn clear_both_sides() {
    let rules = [
        FilterRule::exclude("file.txt").with_sides(true, true),
        FilterRule::clear(), // Clears both sides by default
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Rule is cleared on both sides
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(set.allows_deletion(Path::new("file.txt"), false));
}

// ============================================================================
// Receiver-Only Context Handling
// ============================================================================

/// Verifies receiver context skips sender-only rules.
#[test]
fn receiver_context_skips_sender_only_tail_rule() {
    let rules = [
        FilterRule::exclude("*.tmp").with_sides(false, true), // Receiver exclude
        FilterRule::include("*.tmp").with_sides(true, false), // Sender include (tail)
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Receiver deletion blocked (ignores sender-only include at tail)
    assert!(!set.allows_deletion(Path::new("note.tmp"), false));
}

/// Verifies sender-only risk does not clear receiver protection.
#[test]
fn sender_only_risk_does_not_clear_receiver_protection() {
    let rules = [
        FilterRule::protect("keep/"),
        FilterRule::risk("keep/").with_sides(true, false), // Only applies to sender
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Still protected on receiver
    assert!(!set.allows_deletion(Path::new("keep/item.txt"), false));
}

// ============================================================================
// Complex Multi-Side Scenarios
// ============================================================================

/// Verifies complex scenario with mixed sender/receiver rules.
#[test]
fn complex_mixed_side_scenario() {
    let rules = [
        // Hide internal files from sender
        FilterRule::hide("*.internal"),
        // Exclude temp files on receiver
        FilterRule::exclude("*.tmp").with_sides(false, true),
        // Protect config files on receiver
        FilterRule::protect("*.conf"),
        // Show logs on sender only
        FilterRule::show("logs/**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Internal files hidden from transfer
    assert!(!set.allows(Path::new("secret.internal"), false));

    // Temp files not deletable on receiver
    assert!(!set.allows_deletion(Path::new("scratch.tmp"), false));

    // Config files protected
    assert!(!set.allows_deletion(Path::new("app.conf"), false));

    // Logs visible
    assert!(set.allows(Path::new("logs/app.log"), false));
}

/// Verifies that show/hide don't interfere with receiver operations.
#[test]
fn show_hide_no_receiver_interference() {
    let rules = [
        FilterRule::hide("hidden.txt"),
        FilterRule::show("shown.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Both are freely deletable (show/hide are sender-only)
    assert!(set.allows_deletion(Path::new("hidden.txt"), false));
    assert!(set.allows_deletion(Path::new("shown.txt"), false));
}

/// Verifies that protect/risk don't interfere with sender operations.
#[test]
fn protect_risk_no_sender_interference() {
    let rules = [
        FilterRule::protect("protected.txt"),
        FilterRule::risk("risked.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Both are freely transferable (protect/risk are receiver-only)
    assert!(set.allows(Path::new("protected.txt"), false));
    assert!(set.allows(Path::new("risked.txt"), false));
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies rule with both sides false is effectively disabled.
#[test]
fn rule_with_no_sides_is_disabled() {
    let rule = FilterRule::exclude("*.tmp").with_sides(false, false);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Rule is still compiled (not filtered out like xattr_only),
    // but it won't match anything since both sides are disabled
    assert!(!set.is_empty());

    // The rule doesn't affect transfer or deletion
    assert!(set.allows(Path::new("file.tmp"), false));
    assert!(set.allows_deletion(Path::new("file.tmp"), false));
}

/// Verifies rule can be modified to apply to both sides.
#[test]
fn rule_modified_to_both_sides() {
    let rule = FilterRule::exclude("*.tmp")
        .with_sender(true)
        .with_receiver(true);

    assert!(rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

/// Verifies with_sides overrides previous side settings.
#[test]
fn with_sides_overrides_previous() {
    let rule = FilterRule::show("*.log") // Starts sender-only
        .with_sides(false, true); // Change to receiver-only

    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}
