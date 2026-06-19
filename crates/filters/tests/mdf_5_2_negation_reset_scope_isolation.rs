//! MDF-5.2 gap-cell coverage: a `!` (FILTRULE_CLEAR_LIST) inside a
//! per-directory merge file must clear only that scope's rules, not rules
//! inherited from outer merge files. Closes the row .2 x MDF-5 cell from
//! FIL-AUD-2 (`docs/design/fil-aud-exclude-vs-mdf-matrix.md`).
//!
//! UTS-DD-exclude.2 root cause: the receiver previously allowed a `!` line
//! in a nested `.rsync-filter` to wipe rules from the outer scope's list,
//! diverging from upstream `exclude.c::parse_rule_tok` around the
//! `FILTRULE_CLEAR_LIST` handler (upstream 3.4.1: `exclude.c:1393-1402` in
//! the FIL-AUD-3 spec; see the existing oc-rsync comment at
//! `crates/filters/src/merge/read.rs:87` and the implementation at
//! `crates/filters/src/set.rs::apply_clear_rule`). `pop_filter_list(listp)`
//! only touches the local-scope rules between `head` and `tail` and leaves
//! the inherited list alone.
//!
//! FIL-AUD-3 spec section 2.2.

use std::fs;
use std::path::Path;

use filters::{DirMergeConfig, FilterChain, FilterRule, FilterSet};
use tempfile::TempDir;

/// `!` inside `inner/.rsync-filter` must only clear rules accumulated within
/// that file's scope. The outer `.rsync-filter` exclude of `*.outer` survives
/// while the new exclude `- *.inner` (parsed after the clear) takes effect
/// alongside the still-active outer rule.
///
/// upstream: exclude.c:1393-1402 - FILTRULE_CLEAR_LIST in parse_rule_tok
/// runs pop_filter_list on the local list only.
#[test]
fn inner_bang_clear_does_not_wipe_outer_scope() {
    let root = TempDir::new().unwrap();
    let src = root.path().join("src");
    let inner = src.join("inner");
    fs::create_dir(&src).unwrap();
    fs::create_dir(&inner).unwrap();
    fs::write(src.join(".rsync-filter"), "- *.outer\n").unwrap();
    fs::write(inner.join(".rsync-filter"), "!\n- *.inner\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let src_guard = chain.enter_directory(&src).unwrap();
    let inner_guard = chain.enter_directory(&inner).unwrap();

    assert!(
        !chain.allows(Path::new("f.inner"), false),
        "inner's `- *.inner` (parsed after the clear) must fire",
    );
    assert!(
        !chain.allows(Path::new("f.outer"), false),
        "outer scope's `- *.outer` must survive inner's local `!`",
    );

    chain.leave_directory(inner_guard);
    assert!(
        !chain.allows(Path::new("f.outer"), false),
        "after popping inner, outer scope's rule still applies in src",
    );

    chain.leave_directory(src_guard);
    assert!(
        chain.allows(Path::new("f.outer"), false),
        "after popping all scopes, no rule fires",
    );
}

/// Negative control: a top-level `!` (Clear) rule supplied directly (not
/// via a merge file) MUST still wipe the global include/exclude list,
/// mirroring upstream's non-merge `FILTRULE_CLEAR_LIST` path. This guards
/// against an overcorrection that scopes ALL `!` clears to per-directory
/// files only.
///
/// upstream: exclude.c:1393-1402 - FILTRULE_CLEAR_LIST at the top level
/// targets the global listp, identical mechanism to the merge-file case
/// but with a different target list.
#[test]
fn top_level_bang_still_clears_global_rules() {
    let rules = [
        FilterRule::exclude("*.junk"),
        FilterRule::clear(),
        FilterRule::exclude("*.bak"),
    ];
    let set = FilterSet::from_rules(rules).expect("rules compile");

    assert!(
        set.allows(Path::new("a.junk"), false),
        "`!` cleared the prior `- *.junk` rule",
    );
    assert!(
        !set.allows(Path::new("a.bak"), false),
        "the post-clear `- *.bak` rule still applies",
    );
}
