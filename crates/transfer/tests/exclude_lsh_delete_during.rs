//! UTS-V3-B regression for the sender filter contract observed by the
//! generator walk during `--exclude-from + --delete-during` over remote
//! shells (the `exclude-lsh` upstream testsuite case).
//!
//! The generator's [`FilterChain::allows`] is the integration point that
//! decides whether a path enters the file list at all. Over-filtering
//! here makes the receiver delete dest entries during `--delete-during`
//! that upstream would have kept, producing the diff diagnosed in the
//! UTS-V3 cluster B failure log:
//!
//! ```text
//! oc-rsync deletes (upstream keeps): ./bar/.filt, ./bar/down/... ,
//!     ./foo/sub/file1, ./bar/down/to/foo/.filt2, ...
//! ```
//!
//! The root cause was the synthetic `bar/**` descendant matcher attached
//! to `- /bar`, firing for every child of `bar/` even though the prior
//! `+ **/bar` rule had already included the directory. Upstream
//! `exclude.c:rule_matches()` has no descendant matching at all - the
//! sender traversal handles descendants implicitly by not descending into
//! excluded directories. This test pins the upstream-faithful behaviour
//! at the API boundary that the generator walk consumes.

use std::path::Path;

use filters::{FilterChain, FilterRule, FilterSet};

/// Filter rules verbatim from the `exclude-lsh` testsuite's
/// `scratch/exclude-from` file (testsuite/exclude-lsh_test.py).
fn exclude_lsh_rules() -> Vec<FilterRule> {
    vec![
        FilterRule::include("**/bar"),
        FilterRule::exclude("/bar"),
        FilterRule::include("foo**too"),
        FilterRule::include("foo/s?b/"),
        FilterRule::exclude("foo/*/"),
        FilterRule::exclude("new/keep/**"),
        FilterRule::exclude("new/lose/***"),
        FilterRule::include("t[o]/"),
        FilterRule::exclude("to"),
        FilterRule::include("file4"),
        FilterRule::exclude("file[2-9]"),
        FilterRule::exclude("/mid/for/foo/extra"),
    ]
}

fn chain() -> FilterChain {
    FilterChain::new(FilterSet::from_rules(exclude_lsh_rules()).expect("rules compile"))
}

/// Paths that the failing UTS-V3-B run reported as deleted by oc-rsync but
/// kept by upstream rsync 3.4.3. The sender's traversal-driven
/// `FilterChain::allows` must keep ALL of them so the file list mirrors
/// upstream and `--delete-during` on the receiver does not remove them.
#[test]
fn sender_traversal_keeps_paths_that_upstream_keeps() {
    let chain = chain();

    // Directory `bar` matches `+ **/bar` first - traversal then enters it.
    assert!(chain.allows(Path::new("bar"), true));

    // Children of `bar/` documented as over-deleted in the cluster B log.
    // `file[2-9]` matches (e.g. `file3`) are intentionally excluded by a
    // separate rule and so are NOT part of the kept set.
    let kept = [
        ("bar/.filt", false),
        ("bar/down", true),
        ("bar/down/to", true),
        ("bar/down/to/home-cvs-exclude", false),
        ("bar/down/to/.filt2", false),
        ("bar/down/to/foo", true),
        ("bar/down/to/foo/.filt2", false),
        ("bar/down/to/foo/file1", false),
        ("bar/down/to/foo/file1.bak", false),
        ("bar/down/to/foo/file4", false),
        ("bar/down/to/foo/+ file3", false),
        ("bar/down/to/foo/file4.junk", false),
        // foo/sub/file1: `+ foo/s?b/` matched the directory; children
        // must NOT trigger `- foo/*/`'s synthetic descendant matcher.
        ("foo/sub/file1", false),
    ];
    for (path, is_dir) in kept {
        assert!(
            chain.allows(Path::new(path), is_dir),
            "sender traversal must keep {path}: over-deletion regression"
        );
    }
}

/// Per-entry paths that upstream and oc-rsync agree to exclude. Pins that
/// the descendants-off fix does not over-correct in the opposite
/// direction - traversal-driven excludes that should fire still fire.
///
/// Note: paths inside a directory the traversal will prune (e.g. the
/// contents of `new/lose/` once `- new/lose/***` excludes the directory)
/// are NOT asserted here. Under upstream semantics those entries are
/// never queried by the sender because the walk never descends in;
/// querying them directly bypasses that pruning step.
#[test]
fn sender_traversal_still_excludes_intended_paths() {
    let chain = chain();

    // `- new/keep/**` is a user-written `**` direct matcher, not a
    // synthetic descendant matcher attached to a literal. It must fire
    // for any traversed entry in both modes.
    assert!(!chain.allows(Path::new("new/keep/this"), false));
    // `- new/lose/***` excludes the directory itself, so the sender
    // never descends into it.
    assert!(!chain.allows(Path::new("new/lose"), true));
    // `- file[2-9]` blocks `file3` everywhere - unanchored, so it
    // matches at any depth via the implicit `**/` prefix.
    assert!(!chain.allows(Path::new("foo/file2"), false));
    assert!(!chain.allows(Path::new("bar/down/to/foo/file3"), false));
    // `- /mid/for/foo/extra` anchored literal still excludes.
    assert!(!chain.allows(Path::new("mid/for/foo/extra"), false));
    // `- /bar` for the directory itself loses to the earlier `+ **/bar`.
    // This guard makes sure the include wins.
    assert!(chain.allows(Path::new("bar"), true));
}
