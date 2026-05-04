//! Integration tests for the AppleDouble (`._foo`) sidecar exclusion patterns
//! exposed by the `--apple-double-skip` option.
//!
//! macOS writes AppleDouble sidecar files on filesystems that cannot
//! represent extended attributes natively (FAT, exFAT, NFS, SMB) to carry
//! FinderInfo, resource forks, and xattrs alongside their parent file.
//! Replicating them across machines is rarely useful and frequently clutters
//! destinations with stale metadata. The `--apple-double-skip` option appends
//! a single `._*` exclusion to the filter chain. This test module pins down
//! the user-visible behaviour against [`filters::FilterSet`].
//!
//! Reference: Apple Technical Note TN2078 (AppleDouble layout on non-HFS
//! volumes); upstream rsync `exclude.c` for the filter list scaffolding the
//! built-in patterns reuse.

use filters::{
    DEFAULT_APPLE_DOUBLE_PATTERN, FilterRule, FilterSet, apple_double_default_patterns,
    apple_double_exclusion_rules,
};
use std::path::Path;

/// The default pattern set surfaces the canonical `._*` glob.
#[test]
fn default_pattern_is_dot_underscore() {
    let patterns: Vec<&str> = apple_double_default_patterns().collect();
    assert_eq!(patterns, vec![DEFAULT_APPLE_DOUBLE_PATTERN]);
    assert_eq!(DEFAULT_APPLE_DOUBLE_PATTERN, "._*");
}

/// A single canonical pattern keeps the rule list small and predictable.
#[test]
fn default_patterns_count_is_one() {
    assert_eq!(apple_double_default_patterns().count(), 1);
}

/// `apple_double_exclusion_rules` produces exclude rules with no surprises.
#[test]
fn exclusion_rules_are_excludes() {
    let rules: Vec<FilterRule> = apple_double_exclusion_rules(false).collect();
    assert!(!rules.is_empty());
    for rule in &rules {
        assert_eq!(rule.action(), filters::FilterAction::Exclude);
    }
}

/// The perishable flag toggles correctly between true and false.
#[test]
fn exclusion_rules_perishable_flag_round_trips() {
    let perishable: Vec<FilterRule> = apple_double_exclusion_rules(true).collect();
    let plain: Vec<FilterRule> = apple_double_exclusion_rules(false).collect();
    assert!(perishable.iter().all(|rule| rule.is_perishable()));
    assert!(plain.iter().all(|rule| !rule.is_perishable()));
}

/// AppleDouble sidecars are excluded at the top level.
#[test]
fn filter_set_excludes_top_level_sidecars() {
    let set = FilterSet::from_rules_with_apple_double(Vec::<FilterRule>::new(), false).unwrap();
    assert!(!set.allows(Path::new("._notes.txt"), false));
    assert!(!set.allows(Path::new("._photo.jpg"), false));
    assert!(!set.allows(Path::new("._DS_Store"), false));
}

/// AppleDouble sidecars are excluded at any depth.
#[test]
fn filter_set_excludes_nested_sidecars() {
    let set = FilterSet::from_rules_with_apple_double(Vec::<FilterRule>::new(), false).unwrap();
    assert!(!set.allows(Path::new("photos/._holiday.jpg"), false));
    assert!(!set.allows(Path::new("a/b/c/._deeply_buried"), false));
}

/// Files that merely start with a single dot are not affected.
#[test]
fn filter_set_allows_single_dot_files() {
    let set = FilterSet::from_rules_with_apple_double(Vec::<FilterRule>::new(), false).unwrap();
    assert!(set.allows(Path::new(".bashrc"), false));
    assert!(set.allows(Path::new(".gitignore"), false));
    assert!(set.allows(Path::new(".hidden"), false));
}

/// Files whose names happen to contain `._` mid-string still pass.
#[test]
fn filter_set_allows_embedded_dot_underscore() {
    let set = FilterSet::from_rules_with_apple_double(Vec::<FilterRule>::new(), false).unwrap();
    assert!(set.allows(Path::new("foo._bar"), false));
    assert!(set.allows(Path::new("dir/foo._bar"), false));
}

/// Normal data files always pass.
#[test]
fn filter_set_allows_normal_files() {
    let set = FilterSet::from_rules_with_apple_double(Vec::<FilterRule>::new(), false).unwrap();
    assert!(set.allows(Path::new("notes.txt"), false));
    assert!(set.allows(Path::new("subdir/inner.txt"), false));
    assert!(set.allows(Path::new("Photo.jpg"), false));
}

/// An explicit include placed before the AppleDouble rules wins under the
/// first-match-wins evaluation strategy.
#[test]
fn explicit_include_overrides_apple_double() {
    let rules = vec![FilterRule::include("._keep")];
    let set = FilterSet::from_rules_with_apple_double(rules, false).unwrap();
    assert!(set.allows(Path::new("._keep"), false));
    // Other sidecars remain excluded.
    assert!(!set.allows(Path::new("._other"), false));
}

/// Perishable rules continue to apply during transfers but are skipped during
/// deletion passes, matching `--cvs-exclude` semantics.
#[test]
fn perishable_rules_skip_deletion_pass() {
    let set = FilterSet::from_rules_with_apple_double(Vec::<FilterRule>::new(), true).unwrap();
    assert!(!set.allows(Path::new("._notes.txt"), false));
    assert!(set.allows_deletion(Path::new("._notes.txt"), false));
}

/// Non-perishable rules apply to both transfer and deletion passes.
#[test]
fn non_perishable_rules_apply_to_deletion() {
    let set = FilterSet::from_rules_with_apple_double(Vec::<FilterRule>::new(), false).unwrap();
    assert!(!set.allows(Path::new("._notes.txt"), false));
    assert!(!set.allows_deletion(Path::new("._notes.txt"), false));
}

/// Combining AppleDouble with a clear rule keeps the built-in exclusion
/// active because the clear only affects user-supplied rules.
#[test]
fn clear_rule_does_not_remove_apple_double() {
    let rules = vec![FilterRule::exclude("*.custom"), FilterRule::clear()];
    let set = FilterSet::from_rules_with_apple_double(rules, false).unwrap();
    assert!(set.allows(Path::new("file.custom"), false));
    assert!(!set.allows(Path::new("._sidecar"), false));
}
