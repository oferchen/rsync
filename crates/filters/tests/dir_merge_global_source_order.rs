//! Source-order interleaving of global rules and a `dir-merge` scope.
//!
//! upstream: exclude.c:1043-1064 `check_filter()` walks ONE rule list and,
//! on reaching a `FILTRULE_PERDIR_MERGE` entry, recurses into that entry's
//! mergelist AT THAT POSITION, returning on the first match. A global rule
//! defined BEFORE the `dir-merge` directive therefore wins over rules loaded
//! from the per-directory merge file, and one defined AFTER loses to them.
//!
//! Concretely, for `--filter='- *.tmp' --filter='dir-merge .rsf'` where a
//! subdirectory's `.rsf` contains `+ keep.tmp`, upstream EXCLUDES `keep.tmp`
//! (the global `- *.tmp` precedes the directive). Reversing the two makes the
//! per-directory `+ keep.tmp` win. `DirMergeConfig::with_directive_order`
//! records the directive's position so the chain reproduces both outcomes.

use std::fs;
use std::path::Path;

use filters::{DirMergeConfig, FilterChain, FilterRule, FilterSet};
use tempfile::TempDir;

/// Builds `root/sub/.rsf` containing `+ keep.tmp` and returns the temp dir.
fn setup() -> TempDir {
    let root = TempDir::new().unwrap();
    let sub = root.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join(".rsf"), "+ keep.tmp\n").unwrap();
    root
}

/// A global `- *.tmp` written BEFORE the `dir-merge .rsf` directive
/// (directive_order = 1) wins: `keep.tmp` stays excluded despite the per-dir
/// `+ keep.tmp`, matching upstream's single-list first-match.
#[test]
fn global_exclude_before_dir_merge_wins_over_per_dir_include() {
    let root = setup();
    let global = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
    let mut chain = FilterChain::new(global);
    chain.add_merge_config(DirMergeConfig::new(".rsf").with_directive_order(1));

    let guard = chain.enter_directory(&root.path().join("sub")).unwrap();
    assert_eq!(chain.scope_depth(), 1, "the .rsf scope must be active");

    // Transfer: the global `- *.tmp` (order 0) precedes the directive (position
    // 1), so it decides before the per-dir `+ keep.tmp` - keep.tmp is excluded.
    assert!(
        !chain.allows(Path::new("keep.tmp"), false),
        "global `- *.tmp` before the dir-merge directive must exclude keep.tmp",
    );
    // A .tmp the merge file does not mention is excluded in either ordering.
    assert!(!chain.allows(Path::new("drop.tmp"), false));
    // A non-.tmp file matches no rule and is included by default.
    assert!(chain.allows(Path::new("notes.txt"), false));

    // Deletion mirrors the transfer verdict: the global exclude protects
    // keep.tmp from the receiver's delete pass.
    assert!(
        !chain.allows_deletion(Path::new("keep.tmp"), false),
        "global exclude before the directive must protect keep.tmp from deletion",
    );

    chain.leave_directory(guard);
}

/// The same rules with the `dir-merge .rsf` directive written FIRST
/// (directive_order = 0): the per-directory `+ keep.tmp` now precedes the
/// global `- *.tmp` and wins, so keep.tmp is included.
#[test]
fn per_dir_include_before_global_exclude_wins() {
    let root = setup();
    let global = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
    let mut chain = FilterChain::new(global);
    chain.add_merge_config(DirMergeConfig::new(".rsf").with_directive_order(0));

    let guard = chain.enter_directory(&root.path().join("sub")).unwrap();

    // Transfer: the directive sits at position 0, ahead of the global exclude,
    // so the per-dir `+ keep.tmp` is the first match - keep.tmp is included.
    assert!(
        chain.allows(Path::new("keep.tmp"), false),
        "dir-merge before the global exclude must let `+ keep.tmp` win",
    );
    // The merge file only excepts keep.tmp; other .tmp files stay excluded.
    assert!(!chain.allows(Path::new("drop.tmp"), false));
    assert!(chain.allows(Path::new("notes.txt"), false));

    // Deletion: the per-dir include makes keep.tmp deletable, matching upstream.
    assert!(
        chain.allows_deletion(Path::new("keep.tmp"), false),
        "per-dir `+ keep.tmp` before the global exclude must make keep.tmp deletable",
    );

    chain.leave_directory(guard);
}

/// Regression guard for the default: a chain built without recording a
/// directive position keeps the historical "per-directory scope overrides
/// global" behaviour (directive_order defaults to 0).
#[test]
fn default_directive_order_keeps_per_dir_override() {
    let root = setup();
    let global = FilterSet::from_rules([FilterRule::exclude("*.tmp")]).unwrap();
    let mut chain = FilterChain::new(global);
    chain.add_merge_config(DirMergeConfig::new(".rsf"));

    let guard = chain.enter_directory(&root.path().join("sub")).unwrap();
    assert!(
        chain.allows(Path::new("keep.tmp"), false),
        "with no recorded directive position the per-dir include still wins",
    );
    chain.leave_directory(guard);
}
