//! Integration tests for filter edge cases and complex patterns.
//!
//! These tests verify correct handling of edge cases, unusual patterns,
//! error conditions, and complex pattern combinations that might arise
//! in real-world usage.
//!
//! Reference: rsync 3.4.1 exclude.c for pattern handling edge cases.

use filters::{FilterAction, FilterRule, FilterSet};
use std::path::Path;

// ============================================================================
// Empty and Minimal Filter Tests
// ============================================================================

/// Verifies empty rule set allows everything.
#[test]
fn empty_rules_allow_everything() {
    let set = FilterSet::from_rules(Vec::<FilterRule>::new()).unwrap();

    assert!(set.is_empty());
    assert!(set.allows(Path::new("any/path/file.txt"), false));
    assert!(set.allows_deletion(Path::new("any/path/file.txt"), false));
}

/// Verifies default filter set is empty and allows everything.
#[test]
fn default_filter_set_allows_everything() {
    let set = FilterSet::default();

    assert!(set.is_empty());
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(set.allows_deletion(Path::new("file.txt"), false));
}

/// Verifies single include rule.
#[test]
fn single_include_rule() {
    let set = FilterSet::from_rules([FilterRule::include("*.txt")]).unwrap();

    assert!(!set.is_empty());
    assert!(set.allows(Path::new("readme.txt"), false));
}

/// Verifies single exclude rule.
#[test]
fn single_exclude_rule() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.bak")]).unwrap();

    assert!(!set.is_empty());
    assert!(!set.allows(Path::new("file.bak"), false));
}

// ============================================================================
// Pattern Syntax Edge Cases
// ============================================================================

/// Verifies pattern with only wildcard.
#[test]
fn pattern_only_wildcard() {
    let set = FilterSet::from_rules([FilterRule::exclude("*")]).unwrap();

    // Matches everything
    assert!(!set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("dir"), true));
    assert!(!set.allows(Path::new("a/b/c"), false));
}

/// Verifies pattern with double-star only.
#[test]
fn pattern_only_double_star() {
    let set = FilterSet::from_rules([FilterRule::exclude("**")]).unwrap();

    // Matches everything including paths
    assert!(!set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("a/b/c/file.txt"), false));
}

/// Verifies pattern with question mark only.
#[test]
fn pattern_only_question_mark() {
    let set = FilterSet::from_rules([FilterRule::exclude("?")]).unwrap();

    // Matches single character names
    assert!(!set.allows(Path::new("a"), false));
    assert!(!set.allows(Path::new("1"), false));

    // Does not match longer names
    assert!(set.allows(Path::new("ab"), false));
    assert!(set.allows(Path::new(""), false));
}

/// Verifies pattern with trailing double-star.
#[test]
fn trailing_double_star() {
    let set = FilterSet::from_rules([FilterRule::exclude("src/**")]).unwrap();

    // Matches all contents of src
    assert!(!set.allows(Path::new("src/file.txt"), false));
    assert!(!set.allows(Path::new("src/a/b/c"), false));

    // Does not match src itself by pattern (but might by directory rule logic)
    // The pattern src/** specifically means contents
    assert!(set.allows(Path::new("src"), true));
}

/// Verifies pattern with leading and trailing slashes.
#[test]
fn anchored_directory_only() {
    let set = FilterSet::from_rules([FilterRule::exclude("/build/")]).unwrap();

    // Only matches /build directory at root
    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("build/output"), false));

    // File named build at root is allowed
    assert!(set.allows(Path::new("build"), false));

    // Nested build directory is allowed
    assert!(set.allows(Path::new("src/build"), true));
}

/// Verifies empty pattern handling.
#[test]
fn empty_pattern_in_include() {
    // Empty pattern in include should still create a valid rule
    // (though it might not match anything useful)
    let set = FilterSet::from_rules([FilterRule::include("")]).unwrap();

    // Empty pattern typically matches nothing or root
    assert!(!set.is_empty());
}

// ============================================================================
// Character Class Edge Cases
// ============================================================================

/// Verifies character class with single character.
#[test]
fn character_class_single_char() {
    let set = FilterSet::from_rules([FilterRule::exclude("[a]")]).unwrap();

    assert!(!set.allows(Path::new("a"), false));
    assert!(set.allows(Path::new("b"), false));
}

/// Verifies character class with hyphen at start.
#[test]
fn character_class_hyphen_start() {
    let set = FilterSet::from_rules([FilterRule::exclude("[-a]")]).unwrap();

    assert!(!set.allows(Path::new("-"), false));
    assert!(!set.allows(Path::new("a"), false));
    assert!(set.allows(Path::new("b"), false));
}

/// Verifies character class with hyphen at end.
#[test]
fn character_class_hyphen_end() {
    let set = FilterSet::from_rules([FilterRule::exclude("[a-]")]).unwrap();

    assert!(!set.allows(Path::new("-"), false));
    assert!(!set.allows(Path::new("a"), false));
    assert!(set.allows(Path::new("b"), false));
}

/// Verifies nested character class (literal brackets in class).
#[test]
fn character_class_with_bracket() {
    // Closing bracket at start of class is literal
    let set = FilterSet::from_rules([FilterRule::exclude("[]a]")]).unwrap();

    assert!(!set.allows(Path::new("]"), false));
    assert!(!set.allows(Path::new("a"), false));
    assert!(set.allows(Path::new("b"), false));
}

/// Verifies negated character class with caret.
#[test]
fn character_class_negation_caret() {
    let set = FilterSet::from_rules([FilterRule::exclude("[^0-9]")]).unwrap();

    // Non-digits excluded
    assert!(!set.allows(Path::new("a"), false));
    assert!(!set.allows(Path::new("X"), false));

    // Digits allowed
    assert!(set.allows(Path::new("5"), false));
}

// ============================================================================
// Escaped Character Tests
// ============================================================================

/// Verifies escaped asterisk is literal.
#[test]
fn escaped_asterisk() {
    let set = FilterSet::from_rules([FilterRule::exclude("file\\*.txt")]).unwrap();

    // Literal asterisk
    assert!(!set.allows(Path::new("file*.txt"), false));

    // Wildcard should not match
    assert!(set.allows(Path::new("file1.txt"), false));
    assert!(set.allows(Path::new("fileX.txt"), false));
}

/// Verifies escaped question mark is literal.
#[test]
fn escaped_question_mark() {
    let set = FilterSet::from_rules([FilterRule::exclude("what\\?")]).unwrap();

    // Literal question mark
    assert!(!set.allows(Path::new("what?"), false));

    // Single char wildcard should not match
    assert!(set.allows(Path::new("whatX"), false));
}

/// Verifies escaped brackets are literal.
#[test]
fn escaped_brackets() {
    let set = FilterSet::from_rules([FilterRule::exclude("array\\[0\\]")]).unwrap();

    // Literal brackets
    assert!(!set.allows(Path::new("array[0]"), false));

    // Character class should not match
    assert!(set.allows(Path::new("array0"), false));
}

/// Verifies escaped backslash is literal.
#[test]
fn escaped_backslash() {
    let set = FilterSet::from_rules([FilterRule::exclude("path\\\\file")]).unwrap();

    // Literal backslash
    assert!(!set.allows(Path::new("path\\file"), false));
}

// ============================================================================
// Path Component Tests
// ============================================================================

/// Verifies path with multiple dots.
#[test]
fn path_with_multiple_dots() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.tar.gz")]).unwrap();

    assert!(!set.allows(Path::new("archive.tar.gz"), false));
    assert!(!set.allows(Path::new("backup.tar.gz"), false));
    assert!(set.allows(Path::new("archive.tar"), false));
    assert!(set.allows(Path::new("archive.gz"), false));
}

/// Verifies hidden files (dot prefix).
#[test]
fn hidden_files() {
    let set = FilterSet::from_rules([FilterRule::exclude(".*")]).unwrap();

    assert!(!set.allows(Path::new(".gitignore"), false));
    assert!(!set.allows(Path::new(".env"), false));
    assert!(!set.allows(Path::new(".hidden"), true));
    assert!(set.allows(Path::new("visible"), false));
}

/// Verifies path with dot-dot component.
#[test]
fn path_with_dot_dot() {
    let set = FilterSet::from_rules([FilterRule::exclude("foo")]).unwrap();

    // Path normalization might affect this
    let path = Path::new("bar/../foo");
    // The actual behavior depends on how paths are normalized
    // This test documents the behavior
    assert!(!set.allows(path, false));
}

/// Verifies very long path.
#[test]
fn very_long_path() {
    let set = FilterSet::from_rules([FilterRule::exclude("**/*.txt")]).unwrap();

    let long_path = format!("{}/file.txt", "a/b/c/d/e/f/g/h/i/j".repeat(10));
    assert!(!set.allows(Path::new(&long_path), false));
}

/// Verifies very long pattern.
#[test]
fn very_long_pattern() {
    let long_name = "x".repeat(200);
    let pattern = format!("{long_name}*.txt");
    let set = FilterSet::from_rules([FilterRule::exclude(&pattern)]).unwrap();

    let matching = format!("{long_name}foo.txt");
    assert!(!set.allows(Path::new(&matching), false));
}

// ============================================================================
// Error Handling Tests
// ============================================================================

/// Verifies invalid pattern reports error.
#[test]
fn invalid_pattern_unclosed_bracket() {
    let result = FilterSet::from_rules([FilterRule::exclude("[")]);
    assert!(result.is_err());

    let error = result.unwrap_err();
    assert_eq!(error.pattern(), "[");
}

/// Verifies error preserves pattern text.
#[test]
fn error_preserves_pattern() {
    let result = FilterSet::from_rules([FilterRule::exclude("[invalid")]);

    match result {
        Err(error) => {
            assert_eq!(error.pattern(), "[invalid");
            assert!(error.to_string().contains("failed to compile"));
        }
        Ok(_) => panic!("Expected error"),
    }
}

/// Verifies valid rules still work after invalid one fails.
#[test]
fn valid_rules_compile_before_invalid() {
    // First rule is valid, second is invalid
    let rules = [FilterRule::exclude("*.txt"), FilterRule::exclude("[")];

    // The whole batch fails because one is invalid
    let result = FilterSet::from_rules(rules);
    assert!(result.is_err());
}

// ============================================================================
// Filter Action Tests
// ============================================================================

/// Verifies all filter actions can be created.
#[test]
fn all_filter_actions() {
    let include = FilterRule::include("*");
    let exclude = FilterRule::exclude("*");
    let protect = FilterRule::protect("*");
    let risk = FilterRule::risk("*");
    let clear = FilterRule::clear();

    assert_eq!(include.action(), FilterAction::Include);
    assert_eq!(exclude.action(), FilterAction::Exclude);
    assert_eq!(protect.action(), FilterAction::Protect);
    assert_eq!(risk.action(), FilterAction::Risk);
    assert_eq!(clear.action(), FilterAction::Clear);
}

/// Verifies filter action display.
#[test]
fn filter_action_display() {
    assert_eq!(FilterAction::Include.to_string(), "include");
    assert_eq!(FilterAction::Exclude.to_string(), "exclude");
    assert_eq!(FilterAction::Protect.to_string(), "protect");
    assert_eq!(FilterAction::Risk.to_string(), "risk");
    assert_eq!(FilterAction::Clear.to_string(), "clear");
}

// ============================================================================
// Clone and Debug Tests
// ============================================================================

/// Verifies FilterRule can be cloned.
#[test]
fn filter_rule_clone() {
    let original = FilterRule::exclude("*.tmp")
        .with_perishable(true)
        .with_sender(true);

    let cloned = original.clone();

    assert_eq!(cloned.action(), original.action());
    assert_eq!(cloned.pattern(), original.pattern());
    assert_eq!(cloned.is_perishable(), original.is_perishable());
    assert_eq!(cloned.applies_to_sender(), original.applies_to_sender());
}

/// Verifies FilterRule implements Debug.
#[test]
fn filter_rule_debug() {
    let rule = FilterRule::exclude("*.tmp");
    let debug = format!("{rule:?}");

    assert!(debug.contains("FilterRule"));
    assert!(debug.contains("Exclude"));
    assert!(debug.contains("*.tmp"));
}

/// Verifies FilterSet can be cloned.
#[test]
fn filter_set_clone() {
    let original = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
    let cloned = original.clone();

    // Both should behave identically
    assert!(!original.allows(Path::new("file.tmp"), false));
    assert!(!cloned.allows(Path::new("file.tmp"), false));
}

/// Verifies FilterSet implements Debug.
#[test]
fn filter_set_debug() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
    let debug = format!("{set:?}");

    assert!(debug.contains("FilterSet"));
}

/// Verifies FilterError can be accessed.
#[test]
fn filter_error_access() {
    let result = FilterSet::from_rules([FilterRule::exclude("[")]);

    if let Err(error) = result {
        assert_eq!(error.pattern(), "[");
        // Error should have source
        assert!(!error.to_string().is_empty());
    } else {
        panic!("Expected error");
    }
}

// ============================================================================
// Complex Combination Tests
// ============================================================================

/// Verifies complex nested patterns.
#[test]
fn complex_nested_patterns() {
    // rsync uses first-match-wins: specific includes/excludes must come before general ones
    let rules = [
        FilterRule::exclude("**/node_modules/.cache/*.tmp"), // Most specific: exclude .tmp files
        FilterRule::include("**/node_modules/.cache/**"),    // Include .cache contents
        FilterRule::exclude("**/node_modules/"),             // General: exclude node_modules
    ];
    let set = FilterSet::from_rules(rules).unwrap();

    // node_modules excluded (third rule matches)
    assert!(!set.allows(Path::new("node_modules/lodash"), false));
    assert!(!set.allows(Path::new("packages/app/node_modules/react"), false));

    // .cache within node_modules included (second rule matches)
    assert!(set.allows(Path::new("node_modules/.cache/data"), false));

    // But .tmp within .cache excluded (first rule matches)
    assert!(!set.allows(Path::new("node_modules/.cache/scratch.tmp"), false));
}

/// Verifies rules with all modifiers.
#[test]
fn rule_with_all_modifiers() {
    let rule = FilterRule::exclude("*.log")
        .with_perishable(true)
        .with_sender(true)
        .with_receiver(false)
        .with_xattr_only(false);

    assert_eq!(rule.action(), FilterAction::Exclude);
    assert_eq!(rule.pattern(), "*.log");
    assert!(rule.is_perishable());
    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
    assert!(!rule.is_xattr_only());
}

/// Verifies anchor_to_root modifier.
#[test]
fn anchor_to_root() {
    let rule = FilterRule::exclude("path").anchor_to_root();

    assert_eq!(rule.pattern(), "/path");

    // Already anchored pattern
    let already_anchored = FilterRule::exclude("/path").anchor_to_root();
    assert_eq!(already_anchored.pattern(), "/path");
}

/// Verifies large number of rules.
#[test]
fn many_rules() {
    let rules: Vec<_> = (0..1000)
        .map(|i| FilterRule::exclude(format!("file{i}.txt")))
        .collect();

    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("file500.txt"), false));
    assert!(set.allows(Path::new("file1001.txt"), false));
}

/// Verifies xattr_only rules are filtered out.
#[test]
fn xattr_only_rules_filtered() {
    let rule = FilterRule::exclude("*.xattr").with_xattr_only(true);
    let set = FilterSet::from_rules([rule]).unwrap();

    // XAttr-only rules are filtered out during compilation
    assert!(set.is_empty());

    // So the pattern doesn't affect file matching
    assert!(set.allows(Path::new("file.xattr"), false));
}

/// Verifies Unicode patterns.
#[test]
fn unicode_patterns() {
    let set = FilterSet::from_rules([FilterRule::exclude("*.txt")]).unwrap();

    // Unicode in path (but ASCII pattern)
    assert!(!set.allows(Path::new("cafe.txt"), false));
}

/// Verifies special characters in patterns.
#[test]
fn special_characters_in_name() {
    // Space in pattern
    let set = FilterSet::from_rules([FilterRule::exclude("my file.txt")]).unwrap();
    assert!(!set.allows(Path::new("my file.txt"), false));

    // Dash in pattern
    let set2 = FilterSet::from_rules([FilterRule::exclude("my-file.txt")]).unwrap();
    assert!(!set2.allows(Path::new("my-file.txt"), false));

    // Underscore in pattern
    let set3 = FilterSet::from_rules([FilterRule::exclude("my_file.txt")]).unwrap();
    assert!(!set3.allows(Path::new("my_file.txt"), false));
}
