//! Regression tests for the `--include=*/ --include=<ext>... --exclude=*`
//! idiom commonly used by RPKI/CRL mirroring tools and similar selective
//! transfers. Replicates the issue reproducer that traversed directories
//! incorrectly when `--exclude=*` swallowed the trailing-slash directory
//! include.
//!
//! upstream: `exclude.c:rule_matches()` enforces `FILTRULE_DIRECTORY` so
//! that `*/` rules only match directory entries, allowing recursion into
//! them even when a later `*` exclude would otherwise hide everything.

use filters::{FilterRule, FilterSet};
use std::path::Path;

/// Builds the exact rule list from the original bug report.
fn rpki_style_rules() -> FilterSet {
    FilterSet::from_rules([
        FilterRule::include("*/"),
        FilterRule::include("*.cer"),
        FilterRule::include("*.crl"),
        FilterRule::include("*.mft"),
        FilterRule::include("*.roa"),
        FilterRule::include("*.asa"),
        FilterRule::include("*.tak"),
        FilterRule::include("*.spl"),
        FilterRule::exclude("*"),
    ])
    .unwrap()
}

#[test]
fn directories_traverse_under_trailing_slash_include() {
    let set = rpki_style_rules();

    // Root-level and nested directories must remain visible so the walker
    // can descend into them and evaluate the file rules inside.
    assert!(set.allows(Path::new("subdir"), true));
    assert!(set.allows(Path::new("deep/nested"), true));
    assert!(set.allows(Path::new("a/b/c/d"), true));
}

#[test]
fn extension_includes_match_at_any_depth() {
    let set = rpki_style_rules();

    assert!(set.allows(Path::new("foo.cer"), false));
    assert!(set.allows(Path::new("bar.crl"), false));
    assert!(set.allows(Path::new("subdir/inner.cer"), false));
    assert!(set.allows(Path::new("deep/nested/cert.roa"), false));
    assert!(set.allows(Path::new("publish/repo/manifest.mft"), false));
}

#[test]
fn unrelated_files_are_excluded_by_terminal_star() {
    let set = rpki_style_rules();

    assert!(!set.allows(Path::new("readme.txt"), false));
    assert!(!set.allows(Path::new("subdir/notes.txt"), false));
    assert!(!set.allows(Path::new("deep/nested/skip.log"), false));
}

#[test]
fn directory_only_include_does_not_swallow_files_with_same_name() {
    let set = rpki_style_rules();

    // A file (not a directory) called "subdir" must not be smuggled in by
    // the `*/` rule. Upstream applies `FILTRULE_DIRECTORY` so the rule
    // does not match when `is_dir` is false.
    assert!(!set.allows(Path::new("subdir"), false));
}

#[test]
fn empty_include_list_with_only_exclude_star_still_drops_everything() {
    // Sanity: removing the directory include collapses the chain to the
    // terminal exclude, which should reject every basename at every depth.
    let set = FilterSet::from_rules([FilterRule::exclude("*")]).unwrap();

    assert!(!set.allows(Path::new("foo.cer"), false));
    assert!(!set.allows(Path::new("nested/foo.cer"), false));
}
