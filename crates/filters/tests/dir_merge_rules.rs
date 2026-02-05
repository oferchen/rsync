//! Tests for dir-merge rules functionality.
//!
//! Dir-merge rules read filter rules per-directory during traversal,
//! allowing for directory-specific filtering rules.

use filters::{FilterAction, FilterRule, FilterSet};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// =============================================================================
// Dir-Merge Rule Construction Tests
// =============================================================================

#[test]
fn dir_merge_basic_construction() {
    let rule = FilterRule::dir_merge(".rsync-filter");
    assert_eq!(rule.action(), FilterAction::DirMerge);
    assert_eq!(rule.pattern(), ".rsync-filter");
    assert!(rule.applies_to_sender());
    assert!(rule.applies_to_receiver());
}

#[test]
fn dir_merge_custom_filename() {
    let rule = FilterRule::dir_merge(".gitignore");
    assert_eq!(rule.action(), FilterAction::DirMerge);
    assert_eq!(rule.pattern(), ".gitignore");
}

#[test]
fn dir_merge_with_path_separator() {
    // Dir-merge patterns should typically just be filenames,
    // but paths are technically allowed
    let rule = FilterRule::dir_merge("filters/.rsync-filter");
    assert_eq!(rule.pattern(), "filters/.rsync-filter");
}

#[test]
fn dir_merge_default_modifiers() {
    let rule = FilterRule::dir_merge(".rsync-filter");
    assert!(!rule.is_perishable());
    assert!(!rule.is_xattr_only());
    assert!(!rule.is_negated());
    assert!(!rule.is_exclude_only());
    assert!(!rule.is_no_inherit());
}

#[test]
fn dir_merge_with_perishable() {
    let rule = FilterRule::dir_merge(".rsync-filter").with_perishable(true);
    assert!(rule.is_perishable());
}

#[test]
fn dir_merge_with_no_inherit() {
    let rule = FilterRule::dir_merge(".rsync-filter").with_no_inherit(true);
    assert!(rule.is_no_inherit());
}

// =============================================================================
// Merge Rule Construction Tests (for comparison)
// =============================================================================

#[test]
fn merge_basic_construction() {
    let rule = FilterRule::merge("/etc/rsync/global.rules");
    assert_eq!(rule.action(), FilterAction::Merge);
    assert_eq!(rule.pattern(), "/etc/rsync/global.rules");
}

#[test]
fn merge_relative_path() {
    let rule = FilterRule::merge("relative/path/rules.txt");
    assert_eq!(rule.pattern(), "relative/path/rules.txt");
}

#[test]
fn merge_default_modifiers() {
    let rule = FilterRule::merge("/path/to/rules");
    assert!(!rule.is_perishable());
    assert!(!rule.is_xattr_only());
    assert!(!rule.is_negated());
    assert!(!rule.is_exclude_only());
    assert!(!rule.is_no_inherit());
}

// =============================================================================
// Dir-Merge vs Merge Distinction Tests
// =============================================================================

#[test]
fn dir_merge_and_merge_are_distinct_actions() {
    let dir_merge_rule = FilterRule::dir_merge(".rsync-filter");
    let merge_rule = FilterRule::merge("/path/to/rules");

    assert_eq!(dir_merge_rule.action(), FilterAction::DirMerge);
    assert_eq!(merge_rule.action(), FilterAction::Merge);
    assert_ne!(dir_merge_rule.action(), merge_rule.action());
}

#[test]
fn dir_merge_typical_use_case() {
    // Dir-merge: filename looked up in each directory
    let rule = FilterRule::dir_merge(".rsync-filter");
    assert_eq!(rule.pattern(), ".rsync-filter");

    // Merge: absolute or relative path to a single file
    let rule2 = FilterRule::merge("/home/user/.rsync/global.rules");
    assert!(rule2.pattern().starts_with('/'));
}

// =============================================================================
// FilterSet Integration Tests
// =============================================================================

#[test]
fn filter_set_with_dir_merge_rule() {
    let rules = [
        FilterRule::dir_merge(".rsync-filter"),
        FilterRule::include("*.txt"),
        FilterRule::exclude("*.bak"),
    ];

    let set = FilterSet::from_rules(rules);
    // The filter set should be created successfully
    // Dir-merge rules are included in the set but evaluated during traversal
    assert!(set.is_ok());
}

#[test]
fn filter_set_multiple_dir_merge_rules() {
    let rules = [
        FilterRule::dir_merge(".rsync-filter"),
        FilterRule::dir_merge(".gitignore"),
        FilterRule::include("**"),
    ];

    let set = FilterSet::from_rules(rules);
    assert!(set.is_ok());
}

#[test]
fn filter_set_dir_merge_with_other_rules() {
    let rules = [
        FilterRule::include("*.rs"),
        FilterRule::dir_merge(".rsync-filter"),
        FilterRule::exclude("target/"),
        FilterRule::dir_merge(".project-filter"),
        FilterRule::protect("/important/"),
    ];

    let set = FilterSet::from_rules(rules);
    assert!(set.is_ok());
}

// =============================================================================
// Merge File Parsing Tests
// =============================================================================

#[test]
fn parse_dir_merge_short_form() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, ": .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

#[test]
fn parse_dir_merge_long_form() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, "dir-merge .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
}

#[test]
fn parse_merge_short_form() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("nested.rules");
    fs::write(&nested, "- *.tmp\n").unwrap();

    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, format!(". {}\n", nested.display())).unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Merge);
}

#[test]
fn parse_merge_long_form() {
    let dir = TempDir::new().unwrap();
    let nested = dir.path().join("nested.rules");
    fs::write(&nested, "- *.tmp\n").unwrap();

    let rules_path = dir.path().join("rules.txt");
    fs::write(&rules_path, format!("merge {}\n", nested.display())).unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::Merge);
}

// =============================================================================
// Recursive Expansion Tests
// =============================================================================

#[test]
fn recursive_expansion_preserves_dir_merge() {
    // Dir-merge rules should NOT be expanded recursively
    // They are evaluated during directory traversal
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");
    fs::write(
        &rules_path,
        ": .rsync-filter\n+ *.txt\n- *.bak\n",
    )
    .unwrap();

    let rules = filters::merge::read_rules_recursive(&rules_path, 10).unwrap();
    assert_eq!(rules.len(), 3);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
    assert_eq!(rules[1].action(), FilterAction::Include);
    assert_eq!(rules[2].action(), FilterAction::Exclude);
}

#[test]
fn recursive_expansion_expands_merge_but_not_dir_merge() {
    let dir = TempDir::new().unwrap();

    // Create a nested rules file
    let nested = dir.path().join("nested.rules");
    fs::write(&nested, "- *.tmp\n").unwrap();

    // Main rules file with both merge and dir-merge
    let rules_path = dir.path().join("rules.txt");
    fs::write(
        &rules_path,
        format!(": .rsync-filter\n. {}\n", nested.display()),
    )
    .unwrap();

    let rules = filters::merge::read_rules_recursive(&rules_path, 10).unwrap();
    // Dir-merge preserved, merge expanded
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
    // The merge rule is expanded to show the nested rules
    assert_eq!(rules[1].action(), FilterAction::Exclude);
    assert_eq!(rules[1].pattern(), "*.tmp");
}

#[test]
fn multiple_dir_merge_rules_preserved() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");
    fs::write(
        &rules_path,
        ": .rsync-filter\n: .gitignore\n: .project-rules\n",
    )
    .unwrap();

    let rules = filters::merge::read_rules_recursive(&rules_path, 10).unwrap();
    assert_eq!(rules.len(), 3);
    for rule in &rules {
        assert_eq!(rule.action(), FilterAction::DirMerge);
    }
    assert_eq!(rules[0].pattern(), ".rsync-filter");
    assert_eq!(rules[1].pattern(), ".gitignore");
    assert_eq!(rules[2].pattern(), ".project-rules");
}

// =============================================================================
// Dir-Merge Rule Modifiers Tests
// =============================================================================

#[test]
fn dir_merge_with_modifiers_via_parsing() {
    let dir = TempDir::new().unwrap();
    let rules_path = dir.path().join("rules.txt");

    // Dir-merge with no-inherit modifier
    fs::write(&rules_path, ":n .rsync-filter\n").unwrap();

    let rules = filters::merge::read_rules(&rules_path).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert!(rules[0].is_no_inherit());
}

#[test]
fn dir_merge_side_specific() {
    let rule = FilterRule::dir_merge(".rsync-filter")
        .with_sides(true, false);
    assert!(rule.applies_to_sender());
    assert!(!rule.applies_to_receiver());
}

// =============================================================================
// Edge Cases
// =============================================================================

#[test]
fn dir_merge_empty_pattern() {
    // While technically allowed at construction time,
    // an empty pattern wouldn't be useful
    let rule = FilterRule::dir_merge("");
    assert_eq!(rule.pattern(), "");
}

#[test]
fn dir_merge_with_spaces_in_filename() {
    let rule = FilterRule::dir_merge("my rsync filter");
    assert_eq!(rule.pattern(), "my rsync filter");
}

#[test]
fn dir_merge_with_special_characters() {
    let rule = FilterRule::dir_merge(".rsync-filter-v2.0");
    assert_eq!(rule.pattern(), ".rsync-filter-v2.0");
}

#[test]
fn dir_merge_unicode_filename() {
    let rule = FilterRule::dir_merge(".rsync-フィルタ");
    assert_eq!(rule.pattern(), ".rsync-フィルタ");
}

#[test]
fn dir_merge_equality() {
    let rule1 = FilterRule::dir_merge(".rsync-filter");
    let rule2 = FilterRule::dir_merge(".rsync-filter");
    let rule3 = FilterRule::dir_merge(".gitignore");

    assert_eq!(rule1, rule2);
    assert_ne!(rule1, rule3);
}

#[test]
fn dir_merge_clone() {
    let rule = FilterRule::dir_merge(".rsync-filter").with_no_inherit(true);
    let cloned = rule.clone();
    assert_eq!(rule, cloned);
    assert!(cloned.is_no_inherit());
}

#[test]
fn dir_merge_debug() {
    let rule = FilterRule::dir_merge(".rsync-filter");
    let debug = format!("{:?}", rule);
    assert!(debug.contains("DirMerge"));
    assert!(debug.contains(".rsync-filter"));
}

// =============================================================================
// Complex Scenarios
// =============================================================================

#[test]
fn mixed_merge_and_dir_merge_ordering() {
    let dir = TempDir::new().unwrap();

    let nested = dir.path().join("nested.rules");
    fs::write(&nested, "+ important.txt\n").unwrap();

    let rules_path = dir.path().join("rules.txt");
    fs::write(
        &rules_path,
        format!(
            "# First dir-merge\n\
             : .rsync-filter\n\
             # Regular rules\n\
             + *.rs\n\
             # Merge file\n\
             . {}\n\
             # Another dir-merge\n\
             : .gitignore\n\
             # Final exclude\n\
             - *\n",
            nested.display()
        ),
    )
    .unwrap();

    let rules = filters::merge::read_rules_recursive(&rules_path, 10).unwrap();

    // Verify order: dir-merge, include, (merged) include, dir-merge, exclude
    assert_eq!(rules.len(), 5);
    assert_eq!(rules[0].action(), FilterAction::DirMerge);
    assert_eq!(rules[0].pattern(), ".rsync-filter");
    assert_eq!(rules[1].action(), FilterAction::Include);
    assert_eq!(rules[1].pattern(), "*.rs");
    assert_eq!(rules[2].action(), FilterAction::Include);
    assert_eq!(rules[2].pattern(), "important.txt");
    assert_eq!(rules[3].action(), FilterAction::DirMerge);
    assert_eq!(rules[3].pattern(), ".gitignore");
    assert_eq!(rules[4].action(), FilterAction::Exclude);
    assert_eq!(rules[4].pattern(), "*");
}

#[test]
fn dir_merge_in_real_world_scenario() {
    // Simulating a typical rsync setup with per-directory rules
    let rules = [
        // Global CVS-style ignores
        FilterRule::exclude("*.o"),
        FilterRule::exclude("*.a"),
        FilterRule::exclude("*.so"),
        // Per-directory rules file
        FilterRule::dir_merge(".rsync-filter"),
        // Global includes
        FilterRule::include("**"),
    ];

    let set = FilterSet::from_rules(rules).unwrap();

    // Test that basic filtering still works
    // (dir-merge effects would only be seen during actual traversal)
    assert!(!set.allows(Path::new("test.o"), false));
    assert!(set.allows(Path::new("test.txt"), false));
}
