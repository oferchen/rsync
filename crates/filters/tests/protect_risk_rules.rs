//! Integration tests for protect and risk filter rules.
//!
//! These tests verify the behavior of protect (`P`) and risk (`R`) rules that
//! control deletion during rsync's `--delete` sweep. Protect rules prevent
//! matching destination paths from being removed, while risk rules undo
//! protection. Both types operate independently of include/exclude decisions.
//!
//! Reference: rsync 3.4.1 exclude.c lines 1180-1207 for protect/risk handling.

use filters::{FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Basic Protect Rule Tests
// ============================================================================

/// Verifies that protect rules block deletion without affecting transfer.
///
/// From rsync man page: "The P is equivalent to -r, which makes the rule
/// apply to the receiving side only and protects matching files/dirs from
/// being deleted."
#[test]
fn protect_rule_allows_transfer_blocks_deletion() {
    let set = FilterSet::from_rules([FilterRule::protect("important.dat")]).unwrap();

    // Transfer is allowed (protect doesn't affect transfer decisions)
    assert!(set.allows(Path::new("important.dat"), false));

    // Deletion is blocked
    assert!(!set.allows_deletion(Path::new("important.dat"), false));
}

/// Verifies protect rules work with wildcard patterns.
#[test]
fn protect_rule_wildcard_pattern() {
    let set = FilterSet::from_rules([FilterRule::protect("*.conf")]).unwrap();

    // All .conf files are protected from deletion
    assert!(!set.allows_deletion(Path::new("nginx.conf"), false));
    assert!(!set.allows_deletion(Path::new("app.conf"), false));
    assert!(!set.allows_deletion(Path::new("config/db.conf"), false));

    // Other files can be deleted
    assert!(set.allows_deletion(Path::new("readme.txt"), false));
}

/// Verifies protect rules with directory patterns include descendants.
#[test]
fn protect_rule_directory_pattern_includes_descendants() {
    let set = FilterSet::from_rules([FilterRule::protect("config/")]).unwrap();

    // Directory itself is protected
    assert!(!set.allows_deletion(Path::new("config"), true));

    // All descendants are protected
    assert!(!set.allows_deletion(Path::new("config/app.yaml"), false));
    assert!(!set.allows_deletion(Path::new("config/nested/file.txt"), false));
}

/// Verifies protect rules match at any depth by default.
#[test]
fn protect_rule_matches_at_any_depth() {
    let set = FilterSet::from_rules([FilterRule::protect("credentials.json")]).unwrap();

    // Root level
    assert!(!set.allows_deletion(Path::new("credentials.json"), false));

    // Nested paths
    assert!(!set.allows_deletion(Path::new("app/credentials.json"), false));
    assert!(!set.allows_deletion(Path::new("deep/nested/credentials.json"), false));
}

/// Verifies anchored protect rules only match at root.
#[test]
fn protect_rule_anchored_matches_only_at_root() {
    let set = FilterSet::from_rules([FilterRule::protect("/data")]).unwrap();

    // Root level is protected
    assert!(!set.allows_deletion(Path::new("data"), false));

    // Nested paths are not protected
    assert!(set.allows_deletion(Path::new("backup/data"), false));
}

// ============================================================================
// Basic Risk Rule Tests
// ============================================================================

/// Verifies that risk rules undo protection.
///
/// From rsync man page: "The R is equivalent to -r, which makes the rule
/// apply to the receiving side and means the matching files/dirs can be
/// deleted (unprotected)."
///
/// With first-match-wins, specific risk rule must come before general protect.
#[test]
fn risk_rule_undoes_protection() {
    // Risk for specific path comes first, then protect for parent
    let rules = [
        FilterRule::risk("archive/tmp/"),
        FilterRule::protect("archive/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Main archive is protected (protect matches)
    assert!(!set.allows_deletion(Path::new("archive/data.zip"), false));

    // But tmp subdirectory is not protected (risk matches first)
    assert!(set.allows_deletion(Path::new("archive/tmp/scratch.txt"), false));
}

/// Verifies risk rule applies to directory descendants.
///
/// With first-match-wins, specific risk rule must come before general protect.
#[test]
fn risk_rule_applies_to_descendants() {
    // Risk for specific path comes first, then protect for parent
    let rules = [
        FilterRule::risk("backups/daily/"),
        FilterRule::protect("backups/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Daily backups can be deleted (risk matches first)
    assert!(set.allows_deletion(Path::new("backups/daily/old.tar.gz"), false));

    // Other backups are still protected (protect matches)
    assert!(!set.allows_deletion(Path::new("backups/weekly/archive.tar.gz"), false));
}

/// Verifies multiple protect/risk rules interact correctly.
///
/// With first-match-wins, most specific rules come first.
#[test]
fn multiple_protect_risk_interactions() {
    // Most specific first: protect keep, then risk temp, then protect all
    let rules = [
        FilterRule::protect("temp/keep/"), // Most specific: protect keep within temp
        FilterRule::risk("temp/"),         // Then: allow deletion of temp
        FilterRule::protect("*"),          // Finally: protect everything else
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Regular files protected (protect * matches)
    assert!(!set.allows_deletion(Path::new("important.doc"), false));

    // Temp files can be deleted (risk temp/ matches first)
    assert!(set.allows_deletion(Path::new("temp/scratch.txt"), false));

    // But keep subdirectory is protected (protect temp/keep/ matches first)
    assert!(!set.allows_deletion(Path::new("temp/keep/important.dat"), false));
}

// ============================================================================
// Protect/Risk with Exclude/Include Interactions
// ============================================================================

/// Verifies protect operates independently of exclude.
#[test]
fn protect_independent_of_exclude() {
    let rules = [
        FilterRule::exclude("*.bak"),
        FilterRule::protect("important.bak"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // File is excluded from transfer
    assert!(!set.allows(Path::new("important.bak"), false));

    // But still protected from deletion
    assert!(!set.allows_deletion(Path::new("important.bak"), false));
}

/// Verifies that exclusion and protection can overlap.
#[test]
fn excluded_protected_file() {
    let rules = [
        FilterRule::exclude("secret/"),
        FilterRule::protect("secret/"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory is excluded from transfer
    assert!(!set.allows(Path::new("secret"), true));

    // But protected from deletion
    assert!(!set.allows_deletion(Path::new("secret"), true));
    assert!(!set.allows_deletion(Path::new("secret/data.txt"), false));
}

/// Verifies that include and protection can work together.
///
/// With first-match-wins, specific include must come before general exclude.
#[test]
fn included_protected_file() {
    // Include specific file first, then exclude all
    let rules = [
        FilterRule::include("config.yaml"),
        FilterRule::exclude("*"),
        FilterRule::protect("config.yaml"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // File is included for transfer (include matches first)
    assert!(set.allows(Path::new("config.yaml"), false));

    // And protected from deletion
    assert!(!set.allows_deletion(Path::new("config.yaml"), false));
}

// ============================================================================
// Protect/Risk Side Application Tests
// ============================================================================

/// Verifies protect rules are receiver-side only by default.
#[test]
fn protect_rule_receiver_side_only() {
    let rule = FilterRule::protect("data/**");

    // Protect rules don't apply to sender (transfer decisions)
    assert!(!rule.applies_to_sender());

    // But do apply to receiver (deletion decisions)
    assert!(rule.applies_to_receiver());
}

/// Verifies risk rules are receiver-side only by default.
#[test]
fn risk_rule_receiver_side_only() {
    let rule = FilterRule::risk("temp/**");

    // Risk rules don't apply to sender
    assert!(!rule.applies_to_sender());

    // But do apply to receiver
    assert!(rule.applies_to_receiver());
}

/// Verifies protect can be modified for sender-side.
#[test]
fn protect_rule_with_sender_side() {
    let rule = FilterRule::protect("data").with_sender(true);

    // Now applies to both sides
    assert!(rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

// ============================================================================
// Protect/Risk with Clear Rule Tests
// ============================================================================

/// Verifies clear rule removes protect rules.
#[test]
fn clear_removes_protect_rules() {
    let rules = [FilterRule::protect("critical/"), FilterRule::clear()];
    let set = FilterSet::from_rules(rules).unwrap();

    // Protection is gone after clear
    assert!(set.allows_deletion(Path::new("critical/data.dat"), false));
}

/// Verifies clear rule affects both include/exclude and protect/risk.
#[test]
fn clear_affects_all_rule_types() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::protect("important/"),
        FilterRule::clear(),
        FilterRule::include("*"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Exclude is cleared, all files allowed
    assert!(set.allows(Path::new("file.tmp"), false));

    // Protection is cleared
    assert!(set.allows_deletion(Path::new("important/data"), false));
}

// ============================================================================
// Edge Cases
// ============================================================================

/// Verifies protect with empty filter set.
#[test]
fn protect_only_no_other_rules() {
    let set = FilterSet::from_rules([FilterRule::protect("secret")]).unwrap();

    // Transfer allowed by default for all
    assert!(set.allows(Path::new("secret"), false));
    assert!(set.allows(Path::new("public"), false));

    // Only secret is protected
    assert!(!set.allows_deletion(Path::new("secret"), false));
    assert!(set.allows_deletion(Path::new("public"), false));
}

/// Verifies risk without protect has no effect.
#[test]
fn risk_without_protect_no_effect() {
    let set = FilterSet::from_rules([FilterRule::risk("deletable")]).unwrap();

    // Everything is deletable anyway (no protection to undo)
    assert!(set.allows_deletion(Path::new("deletable"), false));
    assert!(set.allows_deletion(Path::new("anything"), false));
}

/// Verifies complex nested protect/risk patterns.
///
/// With first-match-wins, most specific rules come first.
#[test]
fn nested_protect_risk_patterns() {
    // Most specific to least specific
    let rules = [
        FilterRule::risk("data/cache/important/temp/"),    // Most specific: temp deletable
        FilterRule::protect("data/cache/important/"),      // Then: important protected
        FilterRule::risk("data/cache/"),                   // Then: cache deletable
        FilterRule::protect("data/"),                      // Finally: data protected
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Top level data protected (protect data/ matches)
    assert!(!set.allows_deletion(Path::new("data/file.dat"), false));

    // Cache can be deleted (risk data/cache/ matches first)
    assert!(set.allows_deletion(Path::new("data/cache/item"), false));

    // Important within cache is protected (protect data/cache/important/ matches first)
    assert!(!set.allows_deletion(Path::new("data/cache/important/file"), false));

    // Temp within important can be deleted (risk data/cache/important/temp/ matches first)
    assert!(set.allows_deletion(Path::new("data/cache/important/temp/scratch"), false));
}

/// Verifies protect with character class patterns.
#[test]
fn protect_with_character_class() {
    let set = FilterSet::from_rules([FilterRule::protect("backup[0-9].tar")]).unwrap();

    assert!(!set.allows_deletion(Path::new("backup1.tar"), false));
    assert!(!set.allows_deletion(Path::new("backup9.tar"), false));
    assert!(set.allows_deletion(Path::new("backupA.tar"), false));
}

/// Verifies protect with double-star pattern.
#[test]
fn protect_with_double_star() {
    let set = FilterSet::from_rules([FilterRule::protect("**/secret/**")]).unwrap();

    assert!(!set.allows_deletion(Path::new("secret/file"), false));
    assert!(!set.allows_deletion(Path::new("deep/secret/file"), false));
    assert!(!set.allows_deletion(Path::new("a/b/secret/c/d"), false));
}

/// Verifies protect rule order determines outcome.
///
/// With first-match-wins, the first matching rule determines the outcome.
#[test]
fn protect_risk_order_matters() {
    // Risk then protect - risk wins (first match)
    let rules1 = [FilterRule::risk("file"), FilterRule::protect("file")];
    let set1 = FilterSet::from_rules(rules1).unwrap();
    assert!(set1.allows_deletion(Path::new("file"), false)); // Not protected (first wins)

    // Protect then risk - protect wins (first match)
    let rules2 = [FilterRule::protect("file"), FilterRule::risk("file")];
    let set2 = FilterSet::from_rules(rules2).unwrap();
    assert!(!set2.allows_deletion(Path::new("file"), false)); // Protected (first wins)
}

// ============================================================================
// Delete-Excluded Interaction Tests
// ============================================================================

/// Verifies allows_deletion_when_excluded_removed behavior.
#[test]
fn delete_excluded_respects_protection() {
    let rules = [
        FilterRule::exclude("*.tmp"),
        FilterRule::protect("keep.tmp"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Regular tmp file can be deleted when excluded are removed
    assert!(set.allows_deletion_when_excluded_removed(Path::new("scratch.tmp"), false));

    // Protected tmp file cannot be deleted even when excluded are removed
    assert!(!set.allows_deletion_when_excluded_removed(Path::new("keep.tmp"), false));
}

/// Verifies non-excluded files are not affected by delete-excluded.
#[test]
fn delete_excluded_non_excluded_files() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();

    // Non-excluded file should not be deleted by delete-excluded
    assert!(!set.allows_deletion_when_excluded_removed(Path::new("important.txt"), false));

    // Excluded file can be deleted
    assert!(set.allows_deletion_when_excluded_removed(Path::new("backup.bak"), false));
}
