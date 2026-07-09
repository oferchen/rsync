//! Integration tests for perishable filter rules.
//!
//! Perishable rules (`p` modifier) are excluded from transfer like any other
//! rule and ALSO protect a matching destination entry from the top-level
//! `--delete` scan. Upstream rsync only skips a perishable rule once
//! `ignore_perishable` is set (exclude.c:1044), which happens exclusively
//! while removing the contents of a directory being deleted wholesale
//! (delete.c:147); the top-level scan runs with it unset, so perishable rules
//! behave identically to non-perishable rules for the deletion decision.

use filters::{FilterRule, FilterSet};
use std::path::Path;

/// Verifies a perishable exclude protects a matching entry from deletion.
///
/// upstream: exclude.c:1044 / delete.c:147 - a perishable rule is only skipped
/// inside a wholly-deleted directory, never during the top-level scan.
#[test]
fn perishable_exclude_protects_during_top_level_deletion() {
    let rule = FilterRule::exclude("*.tmp").with_perishable(true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Transfer is excluded (perishable still applies to transfer)
    assert!(!set.allows(Path::new("scratch.tmp"), false));

    // Deletion is blocked: the perishable exclude protects like a plain exclude.
    assert!(!set.allows_deletion(Path::new("scratch.tmp"), false));
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

/// Verifies a perishable include is honoured during deletion (first-match-wins).
#[test]
fn perishable_include_honoured_during_deletion() {
    // With first-match-wins, include comes before exclude
    let rules = [
        FilterRule::include("keep/**").with_perishable(true),
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer includes keep/** (perishable include applies first)
    assert!(set.allows(Path::new("keep/file.txt"), false));

    // Deletion: the perishable include matches first, so the entry is included
    // (not excluded) and therefore a deletion candidate.
    assert!(set.allows_deletion(Path::new("keep/file.txt"), false));
}

/// Verifies perishable and non-perishable includes interact correctly.
#[test]
fn perishable_and_non_perishable_includes() {
    // With first-match-wins, includes come before exclude
    let rules = [
        FilterRule::include("permanent/**"), // Non-perishable (first)
        FilterRule::include("temporary/**").with_perishable(true), // Perishable
        FilterRule::exclude("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Both are included in transfer
    assert!(set.allows(Path::new("permanent/file.txt"), false));
    assert!(set.allows(Path::new("temporary/file.txt"), false));

    // For deletion: permanent/** matches (non-perishable), deletable
    assert!(set.allows_deletion(Path::new("permanent/file.txt"), false));

    // For temporary: the perishable include matches first, so the entry is
    // included and remains a deletion candidate.
    assert!(set.allows_deletion(Path::new("temporary/file.txt"), false));
}

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

/// Verifies perishable exclude before non-perishable include.
#[test]
fn perishable_exclude_before_include() {
    // With first-match-wins, perishable exclude comes before general include
    let rules = [
        FilterRule::exclude("temp_*.txt").with_perishable(true),
        FilterRule::include("*.txt"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Temp txt files excluded from transfer (perishable applies on sender first)
    assert!(!set.allows(Path::new("temp_scratch.txt"), false));
    // For deletion: the perishable exclude matches first and protects the entry.
    assert!(!set.allows_deletion(Path::new("temp_scratch.txt"), false));

    // Regular txt files included in transfer (second rule)
    assert!(set.allows(Path::new("document.txt"), false));
    // For deletion: perishable exclude doesn't match, include *.txt matches, deletable
    assert!(set.allows_deletion(Path::new("document.txt"), false));
}

/// Verifies non-perishable exclude before perishable include.
#[test]
fn non_perishable_exclude_before_perishable_include() {
    // With first-match-wins, specific excludes come first
    let rules = [
        FilterRule::exclude("secret.txt"), // Specific exclude first
        FilterRule::include("*.txt").with_perishable(true), // Perishable include
        FilterRule::exclude("*"),          // Catch-all exclude
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Secret is excluded from transfer (first rule)
    assert!(!set.allows(Path::new("secret.txt"), false));

    // Regular txt files included in transfer (second rule)
    assert!(set.allows(Path::new("document.txt"), false));

    // Deletion: the perishable include *.txt matches first, so the entry is
    // included and remains a deletion candidate (the catch-all exclude never
    // fires).
    assert!(set.allows_deletion(Path::new("document.txt"), false));
}

/// Verifies perishable with directory pattern.
#[test]
fn perishable_directory_pattern() {
    let rule = FilterRule::exclude("cache/").with_perishable(true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Directory excluded from transfer
    assert!(!set.allows(Path::new("cache"), true));
    assert!(!set.allows(Path::new("cache/file.dat"), false));

    // And protected from deletion (perishable exclude behaves like a plain one).
    assert!(!set.allows_deletion(Path::new("cache"), true));
    assert!(!set.allows_deletion(Path::new("cache/file.dat"), false));
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

    // Both protected from deletion: the perishable `build/` exclude matches
    // first for either path (its descendants) and protects them.
    assert!(!set.allows_deletion(Path::new("build/debug/file"), false));
    assert!(!set.allows_deletion(Path::new("build/release/binary"), false));
}

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

    // Other tmp files are excluded and now protected too (perishable exclude).
    assert!(!set.allows(Path::new("scratch.tmp"), false));
    assert!(!set.allows_deletion(Path::new("scratch.tmp"), false));
}

/// Verifies perishable interacts correctly with risk.
#[test]
fn perishable_with_risk() {
    // With first-match-wins, risk comes before protect
    let rules = [
        FilterRule::exclude("archive/").with_perishable(true),
        FilterRule::risk("archive/old/"), // Risk first
        FilterRule::protect("archive/"),  // Protect second
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // All excluded from transfer (perishable exclude applies on sender)
    assert!(!set.allows(Path::new("archive/current/file"), false));
    assert!(!set.allows(Path::new("archive/old/file"), false));

    // Deletion: the perishable `archive/` exclude now matches first for both
    // paths, so the include/exclude chain reports them excluded and they are
    // protected regardless of the risk/protect ordering.
    assert!(!set.allows_deletion(Path::new("archive/old/file"), false));
    assert!(!set.allows_deletion(Path::new("archive/current/file"), false));
}

/// Verifies perishable with sender-only rule.
#[test]
fn perishable_sender_only() {
    let rule = FilterRule::exclude("*.tmp")
        .with_perishable(true)
        .with_sides(true, false);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Excluded from transfer (sender side)
    assert!(!set.allows(Path::new("file.tmp"), false));

    // Deletable: the rule is sender-only, so it never applies to the receiver's
    // delete pass.
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

    // Deletion: the receiver-side perishable exclude matches and protects.
    assert!(!set.allows_deletion(Path::new("file.tmp"), false));
}

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

    // New perishable rule works: excluded from transfer and protected from delete.
    assert!(!set.allows(Path::new("file.tmp"), false));
    assert!(!set.allows_deletion(Path::new("file.tmp"), false));
}

/// Verifies perishable with empty filter set.
#[test]
fn only_perishable_rule() {
    let rule = FilterRule::exclude("*.tmp").with_perishable(true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // Non-matching files allowed
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(set.allows_deletion(Path::new("file.txt"), false));

    // Matching files excluded from transfer and protected from deletion.
    assert!(!set.allows(Path::new("file.tmp"), false));
    assert!(!set.allows_deletion(Path::new("file.tmp"), false));
}

/// Verifies all rules can be perishable.
#[test]
fn all_perishable_rules() {
    // With first-match-wins, include comes before exclude
    let rules = [
        FilterRule::include("important.tmp").with_perishable(true),
        FilterRule::exclude("*.tmp").with_perishable(true),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Transfer: important.tmp is included (first rule)
    assert!(set.allows(Path::new("important.tmp"), false));
    // Transfer: scratch.tmp is excluded (second rule)
    assert!(!set.allows(Path::new("scratch.tmp"), false));

    // Deletion honours perishable rules like any other: important.tmp matches
    // the include first (deletable), scratch.tmp matches the exclude (protected).
    assert!(set.allows_deletion(Path::new("important.tmp"), false));
    assert!(!set.allows_deletion(Path::new("scratch.tmp"), false));
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
    // With first-match-wins, more specific rules come first
    let rules = [
        FilterRule::exclude("src/**/*.tmp").with_perishable(true), // Most specific
        FilterRule::include("src/**"),                             // Then src include
        FilterRule::include("cache/**").with_perishable(true),     // Then cache include
        FilterRule::exclude("*"),                                  // Catch-all exclude
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // src tmp files excluded from transfer (first rule, perishable applies on
    // sender). Use a nested path: upstream `src/**/*.tmp` needs an intermediate
    // directory (the `**/` consumes a real `/`), so `src/sub/scratch.tmp`
    // exercises the perishable exclude where a top-level `src/scratch.tmp`
    // would not match it at all.
    assert!(!set.allows(Path::new("src/sub/scratch.tmp"), false));
    // For deletion: the perishable exclude matches first and protects the entry.
    assert!(!set.allows_deletion(Path::new("src/sub/scratch.tmp"), false));

    // src files included in transfer (second rule)
    assert!(set.allows(Path::new("src/main.rs"), false));
    // For deletion: include src/** matches (non-perishable), deletable
    assert!(set.allows_deletion(Path::new("src/main.rs"), false));

    // cache files included in transfer (third rule)
    assert!(set.allows(Path::new("cache/data.bin"), false));

    // cache deletion: the perishable include cache/** matches first, so the
    // entry is included and remains a deletion candidate.
    assert!(set.allows_deletion(Path::new("cache/data.bin"), false));
}
