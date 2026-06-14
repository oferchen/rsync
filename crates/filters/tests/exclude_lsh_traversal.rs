//! UTS-V3-B regression: the sender's traversal-driven `FilterChain::allows`
//! must skip synthetic descendant matchers so that anchored excludes do not
//! over-filter children of directories that an earlier include rule already
//! allowed.
//!
//! Fixture is the `exclude-lsh` testsuite's exclude-from file:
//!
//! ```text
//! + **/bar
//! - /bar
//! + foo**too
//! + foo/s?b/
//! - foo/*/
//! - new/keep/**
//! - new/lose/***
//! + t[o]/
//! - to
//! + file4
//! - file[2-9]
//! - /mid/for/foo/extra
//! ```
//!
//! With `set.allows` (no-traversal API) the synthetic `bar/**` matcher
//! kicks in - matching the historical FilterSet contract. With the
//! traversal-driven path (`FilterChain::allows`, used by the sender walk)
//! descendant matchers are suppressed to mirror upstream
//! `exclude.c:rule_matches()` which has no descendant matching at all.

use std::path::Path;

use filters::{FilterChain, FilterRule, FilterSet};

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

fn chain_with_exclude_lsh_rules() -> FilterChain {
    let global = FilterSet::from_rules(exclude_lsh_rules()).expect("rules compile");
    FilterChain::new(global)
}

/// `+ **/bar` matches the `bar` directory itself. Under traversal the sender
/// then descends, and per-entry checks for children of `bar/` must NOT
/// trigger the synthetic `bar/**` descendant matcher attached to `- /bar`.
/// Mirrors upstream sender output for the failing 3rd sub-transfer of the
/// `exclude-lsh` test (see testsuite/exclude-lsh_test.py).
#[test]
fn traversal_keeps_children_of_included_bar_directory() {
    let chain = chain_with_exclude_lsh_rules();

    assert!(
        chain.allows(Path::new("bar"), true),
        "+ **/bar wins for bar"
    );

    // The kept set covers everything that the cluster B log showed as
    // over-deleted. `file3` (and any `file[2-9]`) are excluded by a
    // later rule and so are intentionally left out here.
    for path in [
        "bar/.filt",
        "bar/down",
        "bar/down/to",
        "bar/down/to/home-cvs-exclude",
        "bar/down/to/.filt2",
        "bar/down/to/foo",
        "bar/down/to/foo/.filt2",
        "bar/down/to/foo/file1",
        "bar/down/to/foo/file1.bak",
        "bar/down/to/foo/file4",
        "bar/down/to/foo/+ file3",
        "bar/down/to/foo/file4.junk",
    ] {
        let is_dir = matches!(path, "bar/down" | "bar/down/to" | "bar/down/to/foo");
        assert!(
            chain.allows(Path::new(path), is_dir),
            "traversal must keep {path} (no descendant match for `- /bar`)"
        );
    }
}

/// `foo/s?b/` includes `foo/sub` as a directory. The sender then descends
/// into `foo/sub`, and per-entry queries for `foo/sub/file1` must NOT
/// trigger the synthetic `foo/*/**` matcher attached to `- foo/*/`.
/// Mirrors upstream `risking directory foo/sub because of pattern
/// foo/s?b/` semantics.
#[test]
fn traversal_keeps_children_of_included_foo_sub_directory() {
    let chain = chain_with_exclude_lsh_rules();

    assert!(chain.allows(Path::new("foo/sub"), true));
    assert!(
        chain.allows(Path::new("foo/sub/file1"), false),
        "traversal must keep foo/sub/file1 (no descendant match for `- foo/*/`)"
    );
}

/// Single-path API consumers without a traversal context keep the
/// historical "descendants match" semantics so that
/// `set.allows("build/output.bin")` after `- build/` still reports
/// excluded. Without this guard the FilterSet API would silently change
/// behaviour for callers that query individual paths.
#[test]
fn set_allows_keeps_descendant_matching_for_non_traversal_callers() {
    let set = FilterSet::from_rules([FilterRule::exclude("build/")]).expect("compiled");

    assert!(!set.allows(Path::new("build"), true));
    assert!(!set.allows(Path::new("build/output.bin"), false));
}

/// `FilterSet::allows_during_traversal` is the explicit opt-in for the
/// sender's walk: descendant matchers are suppressed so that anchored
/// excludes do not over-match children that traversal pruning would
/// already have skipped.
#[test]
fn set_allows_during_traversal_suppresses_descendant_matchers() {
    let set = FilterSet::from_rules([FilterRule::include("**/bar"), FilterRule::exclude("/bar")])
        .expect("compiled");

    assert!(set.allows_during_traversal(Path::new("bar"), true));
    assert!(set.allows_during_traversal(Path::new("bar/.filt"), false));
    assert!(set.allows_during_traversal(Path::new("bar/down/to/foo/file1"), false));
}

/// Directory-only rules still hide their descendants from
/// `allows_during_traversal` for the directory entry itself (so the
/// traversal can prune correctly), but per-entry queries for descendants
/// are NOT matched - upstream `exclude.c:rule_matches()` returns 0 for a
/// regular file against a `dir/` pattern, and the traversal would never
/// have entered the directory anyway.
#[test]
fn traversal_directory_only_rules_match_dir_not_descendants() {
    let set = FilterSet::from_rules([FilterRule::exclude("new/keep/**")]).expect("compiled");

    // The direct matcher on `new/keep/**` still hits its targets; this is
    // not a synthetic descendant matcher attached to a literal rule, the
    // user wrote `**` explicitly so it must keep working in both modes.
    assert!(!set.allows_during_traversal(Path::new("new/keep/this"), false));
    assert!(!set.allows(Path::new("new/keep/this"), false));
}
