//! UTS-DD-exclude.5 regression: oc-rsync must not double-wrap unanchored
//! patterns that already contain a `**` element.
//!
//! Upstream rsync (`exclude.c:903-960 rule_matches`) handles an unanchored
//! pattern with a `**` element by calling
//! `wildmatch_array(..., slash_handling = -1)` (`lib/wildmatch.c:316`),
//! which tries the pattern at the start AND after every `/`. There is no
//! pattern-string rewriting at parse time. oc-rsync historically prepended
//! an implicit `**/` to every unanchored pattern to mimic the same
//! semantic, but when the source pattern already contained `**` (e.g.
//! `foo/**/bar`) the prefix compounded into `**/foo/**/bar`, polluting the
//! matcher set with rules upstream never emits and breaking byte-for-byte
//! wire parity on the post-normalisation rule strings.
//!
//! This file pins the wire-equivalent pattern set on a corpus lifted from
//! the upstream `testsuite/exclude.test` (the bare `**/bar` line and the
//! `foo**too` line) plus the `foo/**/bar` shape that triggered the bug.

use std::path::Path;

use filters::{FilterRule, FilterSet};

/// `+ foo**too` from `testsuite/exclude.test:102`: an interior bare `**`
/// must still match across path separators after PR #5751's
/// `normalise_recursive_wildcards` rewrite (`foo**too` -> `foo/**/too`).
/// Guarded here so the UTS-DD-exclude.5 fix doesn't regress UTS-20.
#[test]
fn upstream_exclude_test_bare_interior_double_star_matches_cross_segment() {
    let rules = [FilterRule::include("foo**too"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();
    assert!(set.allows(Path::new("bar/down/to/foo/too"), true));
    assert!(set.allows(Path::new("foo/too"), true));
    assert!(set.allows(Path::new("fooxytoo"), false));
}

/// `+ **/bar` from `testsuite/exclude.test:99`: a leading `**/` already
/// carries the recursion, so the implicit prefix must be skipped. The
/// rule still matches `bar` at any depth.
#[test]
fn upstream_exclude_test_leading_double_star_matches_any_depth() {
    let rules = [FilterRule::include("**/bar"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();
    assert!(set.allows(Path::new("bar"), false));
    assert!(set.allows(Path::new("a/b/bar"), false));
    assert!(!set.allows(Path::new("baz"), false));
}

/// UTS-DD-exclude.5 happy path: `foo/**/bar` matches the rooted form
/// (`foo/.../bar`) that globset emits directly from the user-written
/// pattern. The fix removes the prepended `**/foo/**/bar` variant; the
/// kept matcher continues to honour `**` cross-segment semantics.
#[test]
fn rooted_infix_double_star_pattern_still_matches() {
    let rules = [FilterRule::exclude("foo/**/bar")];
    let set = FilterSet::from_rules(rules).unwrap();
    assert!(!set.allows(Path::new("foo/bar"), false));
    assert!(!set.allows(Path::new("foo/x/bar"), false));
    assert!(!set.allows(Path::new("foo/x/y/bar"), false));
}

/// Unanchored patterns without `**` (the canonical case for the
/// implicit-prefix injection) MUST still get the `**/` prefix and match
/// at any depth. Mirrors `exclude.c:917-922` name-only matching for the
/// `!u.slash_cnt && !FILTRULE_WILD2` branch.
#[test]
fn plain_basename_pattern_still_matches_at_any_depth() {
    let rules = [FilterRule::exclude("bar")];
    let set = FilterSet::from_rules(rules).unwrap();
    assert!(!set.allows(Path::new("bar"), false));
    assert!(!set.allows(Path::new("a/b/bar"), false));
    assert!(set.allows(Path::new("baz"), false));
}
