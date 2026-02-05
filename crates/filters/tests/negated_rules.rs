//! Tests for negated rules edge cases.
//!
//! In rsync filter rules, the `!` modifier inverts the match result:
//! - A negated exclude rule excludes files that do NOT match the pattern
//! - A negated include rule includes files that do NOT match the pattern
//!
//! This allows for "exclude everything except X" patterns.

use filters::{FilterAction, FilterRule, FilterSet};
use std::path::Path;

// =============================================================================
// Basic Negation Tests
// =============================================================================

#[test]
fn negated_exclude_basic() {
    let rules = [
        FilterRule::exclude("*.txt").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Negated exclude: excludes files NOT matching *.txt
    assert!(!set.allows(Path::new("image.png"), false));
    assert!(!set.allows(Path::new("data.json"), false));
    // Files matching *.txt are NOT excluded by this rule
    assert!(set.allows(Path::new("readme.txt"), false));
}

#[test]
fn negated_include_basic() {
    let rules = [
        FilterRule::include("*.rs").with_negate(true),
        FilterRule::exclude("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Negated include: includes files NOT matching *.rs
    assert!(set.allows(Path::new("config.toml"), false));
    assert!(set.allows(Path::new("readme.md"), false));
    // Files matching *.rs are NOT included by this rule
    assert!(!set.allows(Path::new("main.rs"), false));
}

#[test]
fn negation_is_pattern_level() {
    // Negation inverts whether the pattern matches, not the action
    let rule = FilterRule::exclude("*.bak").with_negate(true);
    assert!(rule.is_negated());
    assert_eq!(rule.action(), FilterAction::Exclude);
    // The action is still Exclude, but it applies to non-matching files
}

// =============================================================================
// Negation with Different Actions
// =============================================================================

#[test]
fn negated_protect() {
    let rule = FilterRule::protect("/important/").with_negate(true);
    assert!(rule.is_negated());
    assert_eq!(rule.action(), FilterAction::Protect);
}

#[test]
fn negated_risk() {
    let rule = FilterRule::risk("/temp/").with_negate(true);
    assert!(rule.is_negated());
    assert_eq!(rule.action(), FilterAction::Risk);
}

#[test]
fn negated_hide() {
    let rule = FilterRule::hide("*.secret").with_negate(true);
    assert!(rule.is_negated());
    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn negated_show() {
    let rule = FilterRule::show("*.public").with_negate(true);
    assert!(rule.is_negated());
    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

// =============================================================================
// Negation with Wildcards
// =============================================================================

#[test]
fn negated_star_wildcard() {
    let rules = [
        // Exclude files NOT matching *.txt (i.e., all non-.txt files)
        FilterRule::exclude("*.txt").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("file.rs"), false));
    assert!(!set.allows(Path::new("file"), false));
}

#[test]
fn negated_double_star() {
    let rules = [
        // Exclude files NOT under src/
        FilterRule::exclude("src/**").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Files under src/ match, so they're NOT excluded (negated)
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("src/lib/mod.rs"), false));
    // Files not under src/ don't match, so they ARE excluded (negated)
    assert!(!set.allows(Path::new("tests/test.rs"), false));
    assert!(!set.allows(Path::new("build.rs"), false));
}

#[test]
fn negated_question_mark() {
    let rules = [
        FilterRule::exclude("?.txt").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // Single char + .txt matches, not excluded
    assert!(set.allows(Path::new("a.txt"), false));
    assert!(set.allows(Path::new("x.txt"), false));
    // Multiple chars don't match the pattern, so they are excluded
    assert!(!set.allows(Path::new("ab.txt"), false));
    assert!(!set.allows(Path::new("readme.txt"), false));
}

#[test]
fn negated_character_class() {
    let rules = [
        FilterRule::exclude("[abc].txt").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // a.txt, b.txt, c.txt match, not excluded
    assert!(set.allows(Path::new("a.txt"), false));
    assert!(set.allows(Path::new("b.txt"), false));
    assert!(set.allows(Path::new("c.txt"), false));
    // Other single chars don't match, excluded
    assert!(!set.allows(Path::new("d.txt"), false));
    assert!(!set.allows(Path::new("x.txt"), false));
}

// =============================================================================
// Multiple Negated Rules
// =============================================================================

#[test]
fn multiple_negated_excludes() {
    let rules = [
        FilterRule::exclude("*.txt").with_negate(true),
        FilterRule::exclude("*.rs").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // file.txt: rule 1 pattern matches, negate inverts to NOT match
    // rule 2 (exclude *.rs negated) pattern doesn't match, negate inverts to MATCH
    // So excluded by rule 2 (first-match-wins)
    assert!(!set.allows(Path::new("file.txt"), false));
    // file.rs: rule 1 (exclude *.txt negated) pattern doesn't match, negate inverts to MATCH
    // So excluded by rule 1
    assert!(!set.allows(Path::new("file.rs"), false));
    // file.json: rule 1 pattern doesn't match, negate inverts to MATCH, excluded
    assert!(!set.allows(Path::new("file.json"), false));
}

#[test]
fn negated_and_regular_rules_mixed() {
    let rules = [
        // First, exclude everything except .txt files
        FilterRule::exclude("*.txt").with_negate(true),
        // Then, also explicitly exclude backup files
        FilterRule::exclude("*.bak"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // .txt files: first rule doesn't exclude, second doesn't match, third includes
    assert!(set.allows(Path::new("readme.txt"), false));
    // .bak files: first rule excludes (not .txt)
    assert!(!set.allows(Path::new("file.bak"), false));
    // Other files: first rule excludes (not .txt)
    assert!(!set.allows(Path::new("file.rs"), false));
}

// =============================================================================
// Negation with Anchoring
// =============================================================================

#[test]
fn negated_anchored_pattern() {
    let rules = [
        FilterRule::exclude("/config.ini").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // config.ini at root matches, not excluded
    assert!(set.allows(Path::new("config.ini"), false));
    // config.ini elsewhere doesn't match anchored pattern
    // Pattern doesn't match at all, so negation excludes it
    assert!(!set.allows(Path::new("subdir/config.ini"), false));
    // Other files at root: pattern doesn't match, so negation excludes
    assert!(!set.allows(Path::new("other.ini"), false));
}

#[test]
fn negated_unanchored_with_slash() {
    let rules = [
        FilterRule::exclude("src/*.rs").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // src/*.rs matches, not excluded
    assert!(set.allows(Path::new("src/main.rs"), false));
    assert!(set.allows(Path::new("src/lib.rs"), false));
    // Nested in src doesn't match the pattern
    assert!(!set.allows(Path::new("src/subdir/mod.rs"), false));
    // Outside src
    assert!(!set.allows(Path::new("tests/test.rs"), false));
}

// =============================================================================
// Negation with Directory-Only Patterns
// =============================================================================

#[test]
fn negated_directory_only() {
    let rules = [
        FilterRule::exclude("build/").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // build directory matches, not excluded
    assert!(set.allows(Path::new("build"), true));
    // build as file: directory-only pattern doesn't match files
    // So the file doesn't match, negation means it's excluded
    assert!(!set.allows(Path::new("build"), false));
    // Other directories don't match, so they're excluded
    assert!(!set.allows(Path::new("dist"), true));
}

#[test]
fn negated_anchored_directory_only() {
    let rules = [
        FilterRule::exclude("/target/").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // target at root matches, not excluded
    assert!(set.allows(Path::new("target"), true));
    // target elsewhere: anchored pattern doesn't match
    assert!(!set.allows(Path::new("crates/foo/target"), true));
}

// =============================================================================
// Negation with Modifiers
// =============================================================================

#[test]
fn negated_perishable() {
    let rule = FilterRule::exclude("*.tmp")
        .with_negate(true)
        .with_perishable(true);
    assert!(rule.is_negated());
    assert!(rule.is_perishable());
}

#[test]
fn negated_sender_only() {
    let rule = FilterRule::exclude("*.bak")
        .with_negate(true)
        .with_sides(true, false);
    assert!(rule.is_negated());
    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

#[test]
fn negated_receiver_only() {
    let rule = FilterRule::exclude("*.log")
        .with_negate(true)
        .with_sides(false, true);
    assert!(rule.is_negated());
    assert!(!rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn negated_xattr_only() {
    let rule = FilterRule::exclude("user.*")
        .with_negate(true)
        .with_xattr_only(true);
    assert!(rule.is_negated());
    assert!(rule.is_xattr_only());
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn double_negation_not_supported() {
    // Double negation via with_negate(false) on already negated rule
    let rule = FilterRule::exclude("*.txt")
        .with_negate(true)
        .with_negate(false);
    assert!(!rule.is_negated());
}

#[test]
fn negation_preserved_through_clone() {
    let rule = FilterRule::exclude("*.txt").with_negate(true);
    let cloned = rule.clone();
    assert!(cloned.is_negated());
    assert_eq!(rule, cloned);
}

#[test]
fn negation_affects_equality() {
    let rule1 = FilterRule::exclude("*.txt");
    let rule2 = FilterRule::exclude("*.txt").with_negate(true);
    assert_ne!(rule1, rule2);
}

#[test]
fn negation_in_debug_output() {
    let rule = FilterRule::exclude("*.txt").with_negate(true);
    let debug = format!("{rule:?}");
    assert!(debug.contains("negate: true") || debug.contains("negate"));
}

#[test]
fn negated_empty_pattern() {
    // Edge case: negating an empty pattern
    let rules = [
        FilterRule::exclude("").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules);
    // Should handle gracefully even if behavior is undefined
    assert!(set.is_ok());
}

#[test]
fn negated_star_star() {
    // Negating ** - should effectively match nothing
    let rules = [
        FilterRule::exclude("**").with_negate(true),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // ** matches everything, so negation excludes nothing
    // All files should be included
    assert!(set.allows(Path::new("any_file.txt"), false));
    assert!(set.allows(Path::new("deep/nested/file.rs"), false));
}

// =============================================================================
// Merge File Parsing
// =============================================================================

#[test]
fn parse_negated_exclude_short() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, "-! *.txt\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert!(rules[0].is_negated());
    assert_eq!(rules[0].pattern(), "*.txt");
}

#[test]
fn parse_negated_include_short() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, "+! *.bak\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert!(rules[0].is_negated());
}

#[test]
fn parse_negated_with_other_modifiers() {
    use std::fs;
    use tempfile::TempDir;

    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, "-!ps *.tmp\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].is_negated());
    assert!(rules[0].is_perishable());
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
}

// =============================================================================
// Real-World Scenarios
// =============================================================================

#[test]
fn only_include_specific_extensions() {
    // "Include only .rs and .toml files" pattern
    let rules = [
        FilterRule::include("*.rs"),
        FilterRule::include("*.toml"),
        FilterRule::include("*/"), // Include directories for traversal
        FilterRule::exclude("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("main.rs"), false));
    assert!(set.allows(Path::new("Cargo.toml"), false));
    assert!(set.allows(Path::new("src"), true));
    assert!(!set.allows(Path::new("README.md"), false));
}

#[test]
fn exclude_everything_except_pattern_using_negation() {
    // Alternative approach using negation
    let rules = [
        FilterRule::include("*.rs"),
        FilterRule::include("*.toml"),
        FilterRule::exclude("*.rs").with_negate(true), // Exclude non-.rs
        // Note: This is a somewhat convoluted way to do it
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // .rs files included by first rule
    assert!(set.allows(Path::new("main.rs"), false));
    // .toml files included by second rule
    assert!(set.allows(Path::new("Cargo.toml"), false));
    // Other files excluded by third rule (negated .rs)
    assert!(!set.allows(Path::new("README.md"), false));
}

#[test]
fn backup_except_sensitive() {
    // Backup everything except sensitive files
    let rules = [
        // Don't backup secrets
        FilterRule::exclude("*.key"),
        FilterRule::exclude("*.pem"),
        FilterRule::exclude(".env"),
        FilterRule::exclude("secrets/"),
        // Include everything else
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(set.allows(Path::new("config.yaml"), false));
    assert!(!set.allows(Path::new("server.key"), false));
    assert!(!set.allows(Path::new(".env"), false));
    assert!(!set.allows(Path::new("secrets"), true));
}

#[test]
fn negated_rule_for_exception() {
    // Exclude all .log except error.log
    let rules = [
        FilterRule::include("error.log"),
        FilterRule::exclude("*.log"),
        FilterRule::include("**"),
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // error.log specifically included
    assert!(set.allows(Path::new("error.log"), false));
    // Other logs excluded
    assert!(!set.allows(Path::new("access.log"), false));
    assert!(!set.allows(Path::new("debug.log"), false));
    // Non-logs included
    assert!(set.allows(Path::new("app.rs"), false));
}
