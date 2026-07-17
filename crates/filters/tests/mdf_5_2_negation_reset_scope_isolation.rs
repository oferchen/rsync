//! MDF-5.2 gap-cell coverage: a `!` (FILTRULE_CLEAR_LIST) inside a
//! per-directory merge file clears the WHOLE mergelist, including the rules
//! inherited from outer directories' merge files. Closes the row .2 x MDF-5
//! cell from FIL-AUD-2 (`docs/design/fil-aud-exclude-vs-mdf-matrix.md`).
//!
//! Upstream mechanism (rsync 3.4.4 `exclude.c`): `push_local_filters()` at
//! `exclude.c:801` sets `lp->tail = NULL` when entering a directory, which
//! reclassifies the parent directory's rules as the INHERITED tail of the
//! same mergelist `lp`. The `FILTRULE_CLEAR_LIST` handler
//! (`exclude.c:1399-1400`) then runs `pop_filter_list(listp)` - which
//! early-returns at `exclude.c:579` when `!` is the file's first line because
//! `listp->tail` is NULL - and, crucially, ALSO runs `listp->head = NULL`.
//! That final `head = NULL` drops the inherited parent rules, so a nested `!`
//! resets the entire list, not just the current file's own section. Verified
//! against the real rsync 3.4.4 binary: with `src/.rsync-filter` = `- *.outer`
//! and `src/inner/.rsync-filter` = `!` + `- *.inner`, `rsync -n -r -i -F`
//! transfers `inner/f.outer` (the inherited exclude was cleared) while still
//! excluding `src/top.outer`.
//!
//! (The `.`-style single-shot `merge` path is a distinct mechanism whose `!`
//! is scope-local - see `crates/filters/src/set.rs::apply_clear_rule`; this
//! test covers the per-directory `dir-merge` traversal path in
//! `crates/filters/src/chain/mod.rs`.)
//!
//! FIL-AUD-3 spec section 2.2.

use std::fs;
use std::path::Path;

use filters::{DirMergeConfig, FilterChain, FilterRule, FilterSet};
use tempfile::TempDir;

/// `!` inside `inner/.rsync-filter` clears the whole mergelist for that
/// directory and its descendants, so the inherited outer exclude of `*.outer`
/// no longer fires. The new exclude `- *.inner` (parsed after the clear) still
/// takes effect. When `inner` is left, the outer rule is restored for the rest
/// of `src` (and its siblings), mirroring `exclude.c:pop_local_filters()`.
///
/// upstream: exclude.c:801 push_local_filters (parent rules become inherited)
/// + exclude.c:1399-1400 FILTRULE_CLEAR_LIST (pop_filter_list then head=NULL).
#[test]
fn inner_bang_clear_wipes_inherited_outer_scope() {
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
        chain.allows(Path::new("f.outer"), false),
        "inner's `!` clears the inherited `- *.outer`, so f.outer transfers \
         (real rsync 3.4.4 emits `>f+++++++++ inner/f.outer`)",
    );

    chain.leave_directory(inner_guard);
    assert!(
        !chain.allows(Path::new("f.outer"), false),
        "after popping inner, the outer scope's rule is restored in src",
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
