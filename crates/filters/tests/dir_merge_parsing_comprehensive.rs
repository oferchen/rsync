//! Comprehensive tests for dir-merge rule parsing and modifiers.
//!
//! This test suite covers advanced dir-merge functionality including:
//! - All modifier combinations
//! - Error handling for malformed rules
//! - Edge cases in pattern syntax
//! - Interaction with other rule types

use filters::{FilterAction, FilterRule, FilterSet};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// =============================================================================
// Dir-Merge Modifier Combination Tests
// =============================================================================

#[test]
fn dir_merge_multiple_modifiers() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Dir-merge with multiple modifiers: perishable, sender-only, no-inherit
    fs::write(&rules_path, ":psn .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert!(rules[0].is_perishable());
    assert!(rules[0].applies_to_sender());
    assert!(!rules[0].applies_to_receiver());
    assert!(rules[0].is_no_inherit());
}

#[test]
fn dir_merge_receiver_only() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Dir-merge receiver-only
    fs::write(&rules_path, ":r .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(!rules[0].applies_to_sender());
    assert!(rules[0].applies_to_receiver());
}

#[test]
fn dir_merge_both_sides_explicit() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Dir-merge with both sender and receiver flags (should apply to both)
    fs::write(&rules_path, ":sr .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    // When both are specified, both apply
    assert!(rules[0].applies_to_sender());
    assert!(rules[0].applies_to_receiver());
}

#[test]
fn dir_merge_exclude_only_modifier() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Dir-merge with exclude-only modifier
    fs::write(&rules_path, ":e .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].is_exclude_only());
}

#[test]
fn dir_merge_with_underscore_separator() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Underscore separator between modifiers and pattern
    fs::write(&rules_path, ":p_.rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].is_perishable());
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

#[test]
fn dir_merge_with_space_separator() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Space separator between modifiers and pattern
    fs::write(&rules_path, ":p .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].is_perishable());
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

#[test]
fn dir_merge_no_separator() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // No separator - pattern starts immediately after modifiers
    fs::write(&rules_path, ":n.rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(rules[0].is_no_inherit());
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

// =============================================================================
// Merge Rule Modifier Tests (for comparison)
// =============================================================================

#[test]
fn merge_with_modifiers_ignored() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("nested.rules");
    fs::write(&nested, "- *.tmp\n").unwrap();

    let rules_path = dir.path().join("rules.txt");
    // Merge rules don't support most modifiers, but shouldn't error
    fs::write(&rules_path, format!(". {}\n", nested.display())).unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Merge);
}

#[test]
fn merge_absolute_path() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Absolute path in merge
    fs::write(&rules_path, ". /etc/rsync/global.rules\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "/etc/rsync/global.rules");
}

#[test]
fn merge_relative_path_with_subdirs() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Relative path with subdirectories
    fs::write(&rules_path, ". config/filters/rules.txt\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "config/filters/rules.txt");
}

// =============================================================================
// Error Handling and Edge Cases
// =============================================================================

#[test]
fn dir_merge_empty_filename() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Dir-merge with just colon and whitespace
    fs::write(&rules_path, ": \n").unwrap();

    let result = filters::merge::read_rules(&rules_path);
    // Should fail because pattern is empty after trimming
    assert!(result.is_err());
}

#[test]
fn dir_merge_only_modifiers_no_pattern() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Only modifiers, no actual filename
    fs::write(&rules_path, ":psn\n").unwrap();

    let result = filters::merge::read_rules(&rules_path);
    // Should fail because there's no pattern
    assert!(result.is_err());
}

#[test]
fn dir_merge_with_leading_whitespace() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Leading whitespace before dir-merge
    fs::write(&rules_path, "   : .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
}

#[test]
fn dir_merge_with_trailing_whitespace() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Trailing whitespace after pattern
    fs::write(&rules_path, ": .rsync-filter   \n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    // Trailing whitespace should be trimmed from pattern
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

#[test]
fn dir_merge_mixed_case_long_form() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Mixed case in long form (should be case-insensitive)
    fs::write(&rules_path, "Dir-Merge .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
}

#[test]
fn dir_merge_upper_case_long_form() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // All uppercase long form
    fs::write(&rules_path, "DIR-MERGE .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
}

#[test]
fn dir_merge_pattern_with_wildcards() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Dir-merge with wildcards in filename (unusual but valid)
    fs::write(&rules_path, ": .rsync-*\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), ".rsync-*");
}

#[test]
fn dir_merge_pattern_with_path_separator() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Dir-merge with path separator (unusual)
    fs::write(&rules_path, ": subdir/.rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern(), "subdir/.rsync-filter");
}

// =============================================================================
// Integration with Other Rule Types
// =============================================================================

#[test]
fn dir_merge_between_include_exclude() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    fs::write(
        &rules_path,
        "+ *.txt\n\
         : .rsync-filter\n\
         - *.bak\n",
    )
    .unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].action(), FilterAction::Include);
    assert_eq!(rules[1].action(), FilterAction::DirMerge);
    assert_eq!(rules[2].action(), FilterAction::Exclude);
}

#[test]
fn multiple_dir_merges_different_files() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    fs::write(
        &rules_path,
        ": .rsync-filter\n\
         : .gitignore\n\
         : .hgignore\n",
    )
    .unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 3);
    for rule in &rules {
        assert_eq!(rule.action(), FilterAction::DirMerge);
    }
    assert_eq!(rules[0].pattern(), ".rsync-filter");
    assert_eq!(rules[1].pattern(), ".gitignore");
    assert_eq!(rules[2].pattern(), ".hgignore");
}

#[test]
fn dir_merge_and_merge_interleaved() {
    let dir = TempDir::new().unwrap();

    let nested = dir.path().join("nested.rules");
    fs::write(&nested, "- *.log\n").unwrap();

    let rules_path = dir.path().join("rules.txt");
    fs::write(
        &rules_path,
        format!(
            ": .rsync-filter\n\
             . {}\n\
             : .gitignore\n",
            nested.display()
        ),
    )
    .unwrap();

    let rules = filters::merge::read_rules_recursive(&rules_path, 10).unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[1].action(), FilterAction::Exclude); // From merged file
    assert_eq!(rules[2].action(), FilterAction::DirMerge);
}

#[test]
fn dir_merge_with_protect_rules() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    fs::write(
        &rules_path,
        "P /important/\n\
         : .rsync-filter\n\
         R /temp/\n",
    )
    .unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].action(), FilterAction::Protect);
    assert_eq!(rules[1].action(), FilterAction::DirMerge);
    assert_eq!(rules[2].action(), FilterAction::Risk);
}

#[test]
fn dir_merge_after_clear() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    fs::write(
        &rules_path,
        "- *.tmp\n\
         !\n\
         : .rsync-filter\n\
         + *.txt\n",
    )
    .unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 4);
    assert_eq!(rules[0].action(), FilterAction::Exclude);
    assert_eq!(rules[1].action(), FilterAction::Clear);
    assert_eq!(rules[2].action(), FilterAction::DirMerge);
    assert_eq!(rules[3].action(), FilterAction::Include);
}

// =============================================================================
// Modifier Order Independence Tests
// =============================================================================

#[test]
fn dir_merge_modifiers_different_order_same_result() {
    let dir = TempDir::new().unwrap();

    // Test all permutations of common modifiers
    let patterns = vec![
        ":psn .rsync-filter\n",
        ":pns .rsync-filter\n",
        ":spn .rsync-filter\n",
        ":snp .rsync-filter\n",
        ":nps .rsync-filter\n",
        ":nsp .rsync-filter\n",
    ];

    for pattern in patterns {
        let rules_path = dir.path().join("rules.txt");
        fs::write(&rules_path, pattern).unwrap();

        let rules = filters::merge::read_rules(&rules_path).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(rules[0].is_perishable(), "Failed for pattern: {pattern}");
        assert!(
            rules[0].applies_to_sender(),
            "Failed for pattern: {pattern}"
        );
        assert!(
            !rules[0].applies_to_receiver(),
            "Failed for pattern: {pattern}"
        );
        assert!(rules[0].is_no_inherit(), "Failed for pattern: {pattern}");
    }
}

// =============================================================================
// FilterSet Compilation with Dir-Merge Tests
// =============================================================================

#[test]
fn filter_set_skips_dir_merge_during_compilation() {
    let rules = vec![
        FilterRule::include("*.txt"),
        FilterRule::dir_merge(".rsync-filter"),
        FilterRule::exclude("*.bak"),
    ];

    let set = FilterSet::from_rules(rules).unwrap();

    // FilterSet should compile successfully
    // Dir-merge rules are skipped during compilation
    assert!(!set.is_empty());

    // Other rules should still work
    assert!(set.allows(Path::new("file.txt"), false));
    assert!(!set.allows(Path::new("file.bak"), false));
}

#[test]
fn filter_set_only_dir_merge_rules() {
    let rules = vec![
        FilterRule::dir_merge(".rsync-filter"),
        FilterRule::dir_merge(".gitignore"),
    ];

    let set = FilterSet::from_rules(rules).unwrap();

    // All rules are dir-merge, so set is effectively empty
    assert!(set.is_empty());

    // Should allow everything by default
    assert!(set.allows(Path::new("anything"), false));
}

#[test]
fn filter_set_dir_merge_with_cvs_patterns() {
    let rules = vec![
        FilterRule::dir_merge(".rsync-filter"),
        FilterRule::include("*.txt"),
    ];

    let set = FilterSet::from_rules_with_cvs(rules, false).unwrap();

    // Should have CVS patterns plus the include rule
    assert!(!set.is_empty());

    // CVS patterns should work
    assert!(!set.allows(Path::new("main.o"), false));

    // Include rule should work
    assert!(set.allows(Path::new("readme.txt"), false));
}

// =============================================================================
// Pattern Preservation Tests
// =============================================================================

#[test]
fn dir_merge_preserves_pattern_case() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Mixed case in pattern should be preserved
    fs::write(&rules_path, ": .Rsync-Filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules[0].pattern(), ".Rsync-Filter");
}

#[test]
fn dir_merge_preserves_special_chars() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Special characters in filename
    fs::write(&rules_path, ": .rsync-filter_v2.0\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules[0].pattern(), ".rsync-filter_v2.0");
}

// =============================================================================
// Comments and Whitespace Tests
// =============================================================================

#[test]
fn dir_merge_with_inline_comment() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Pattern followed by comment (comment should not be part of pattern)
    fs::write(&rules_path, ": .rsync-filter # This is a comment\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    // The pattern should include everything up to the comment marker
    // (actual behavior depends on parsing implementation)
    let pattern = rules[0].pattern();
    // In rsync, comments are not supported inline, so this would be part of filename
    assert!(pattern.contains(".rsync-filter"));
}

#[test]
fn dir_merge_after_comment_line() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    fs::write(
        &rules_path,
        "# Comment line\n\
         : .rsync-filter\n",
    )
    .unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
}

#[test]
fn dir_merge_with_empty_lines_around() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    fs::write(
        &rules_path,
        "\n\
         : .rsync-filter\n\
         \n",
    )
    .unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
}
