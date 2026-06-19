//! MDF-5.1 gap-cell coverage: per-directory scope fall-through on the
//! Deletion context must not synthesise descendants from a sibling scope
//! that has already been popped.
//!
//! Closes the row .1 x MDF-5 cell from FIL-AUD-2
//! (`docs/design/fil-aud-exclude-vs-mdf-matrix.md`): UTS-DD-exclude.1
//! root-caused a Deletion-side bug where rules from a sibling `.rsync-filter`
//! continued to fire after `leave_directory` because the deletion code path
//! was promoting `pattern/**` descendant matchers across scopes. Upstream
//! `exclude.c::rule_matches()` (3.4.1, lines 903-960) has NO descendant
//! matching at all; `exclude.c::check_filter()` (around lines 770-820) walks
//! the active per-scope lists only and falls through to outer scopes when
//! the active scope is silent on the path. The in-tree wiring lives at
//! `crates/filters/src/decision.rs` (DecisionContext::Deletion branch) and
//! `crates/filters/src/chain/mod.rs::allows_deletion`.
//!
//! Without this regression test the matrix's row .1 x MDF-5 cell relied on
//! the single-path UTS-DD-exclude.1 regression which does not exercise the
//! per-directory chain. FIL-AUD-3 spec section 2.1.

use std::fs;
use std::path::Path;

use filters::{DirMergeConfig, FilterChain};
use tempfile::TempDir;

/// Sibling per-dir scopes must not leak Deletion rules across `leave_directory`.
///
/// `alpha/.rsync-filter` excludes `*.tmp`. After leaving `alpha/` and entering
/// `beta/` (which has no merge file), the chain must report `b.tmp` as
/// allowed-for-deletion: no scope is active that names it. A regression that
/// promoted `alpha`'s `*.tmp` rule into a synthetic descendant would mark
/// `b.tmp` as excluded and skip it from the receiver's delete pass.
///
/// upstream: exclude.c::check_filter() walks the active scope list; popping
/// a scope clears its rules.
#[test]
fn sibling_scope_does_not_leak_deletion_rules() {
    let root = TempDir::new().unwrap();
    let alpha = root.path().join("alpha");
    let beta = root.path().join("beta");
    fs::create_dir(&alpha).unwrap();
    fs::create_dir(&beta).unwrap();
    fs::write(alpha.join(".rsync-filter"), "- *.tmp\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let alpha_guard = chain.enter_directory(&alpha).unwrap();
    assert_eq!(chain.scope_depth(), 1, "alpha scope must be active");
    assert!(
        !chain.allows_deletion(Path::new("a.tmp"), false),
        "alpha's `- *.tmp` must block deletion of a.tmp while inside alpha",
    );
    assert!(
        chain.allows_deletion(Path::new("a.keep"), false),
        "alpha's rule must not affect a.keep",
    );
    chain.leave_directory(alpha_guard);
    assert_eq!(chain.scope_depth(), 0, "alpha scope must pop on leave");

    let beta_guard = chain.enter_directory(&beta).unwrap();
    assert_eq!(
        chain.scope_depth(),
        0,
        "beta has no merge file so no scope is pushed",
    );
    assert!(
        chain.allows_deletion(Path::new("b.tmp"), false),
        "after popping alpha, no scope names *.tmp so b.tmp must be deletable",
    );
    assert!(
        chain.allows_deletion(Path::new("b.keep"), false),
        "b.keep must remain deletable",
    );
    chain.leave_directory(beta_guard);
}

/// Transfer-context parity: the same fixture must behave identically on the
/// Transfer side. This sanity check isolates any future regression to the
/// Deletion branch (`decision.rs::Deletion`) rather than a general scope-leak
/// in the per-directory chain.
///
/// upstream: exclude.c::rule_matches() / check_filter() share the rule walk
/// across Transfer and Deletion contexts; the only difference is which side
/// flag (`applies_to_sender` vs `applies_to_receiver`) the predicate consults.
#[test]
fn sibling_scope_does_not_leak_transfer_rules() {
    let root = TempDir::new().unwrap();
    let alpha = root.path().join("alpha");
    let beta = root.path().join("beta");
    fs::create_dir(&alpha).unwrap();
    fs::create_dir(&beta).unwrap();
    fs::write(alpha.join(".rsync-filter"), "- *.tmp\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let alpha_guard = chain.enter_directory(&alpha).unwrap();
    assert!(!chain.allows(Path::new("a.tmp"), false));
    assert!(chain.allows(Path::new("a.keep"), false));
    chain.leave_directory(alpha_guard);

    let beta_guard = chain.enter_directory(&beta).unwrap();
    assert!(
        chain.allows(Path::new("b.tmp"), false),
        "Transfer-context: alpha scope is popped, b.tmp must transfer",
    );
    chain.leave_directory(beta_guard);
}

/// Negative control: a child directory inheriting through `enter_directory`
/// MUST still see the parent merge rule on the Deletion path. This guards
/// against an overcorrection that drops inheritance entirely when fixing
/// the sibling-leak path. Upstream `push_local_filters` keeps the inheriting
/// list visible to descendants.
///
/// upstream: exclude.c::push_local_filters() lp->head is preserved across
/// descend for inheriting rules.
#[test]
fn nested_scope_keeps_inherited_deletion_rule() {
    let root = TempDir::new().unwrap();
    let alpha = root.path().join("alpha");
    let inner = alpha.join("inner");
    fs::create_dir(&alpha).unwrap();
    fs::create_dir(&inner).unwrap();
    fs::write(alpha.join(".rsync-filter"), "- *.tmp\n").unwrap();

    let mut chain = FilterChain::empty();
    chain.add_merge_config(DirMergeConfig::new(".rsync-filter"));

    let alpha_guard = chain.enter_directory(&alpha).unwrap();
    let inner_guard = chain.enter_directory(&inner).unwrap();

    assert!(
        !chain.allows_deletion(Path::new("c.tmp"), false),
        "inner inherits alpha's `- *.tmp`; deletion must still be blocked",
    );

    chain.leave_directory(inner_guard);
    chain.leave_directory(alpha_guard);
}
