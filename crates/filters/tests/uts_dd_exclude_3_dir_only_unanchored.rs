//! UTS-DD-exclude.3 regression: a directory-only unanchored pattern such
//! as `- foo/*/` must NOT auto-synthesise `/**` descendants. Upstream
//! `exclude.c:rule_matches()` returns "no match" for a non-directory
//! candidate when the rule carries `FILTRULE_DIRECTORY` (line 938-939),
//! so the file is included by default and the sender's walk handles
//! descent pruning by never entering the matched directory. Pre-baking
//! `foo/*/**` would steal precedence from a sibling include like
//! `+ foo/s?b/` and over-delete on the receiver's single-path API.
//!
//! Fixture mirrors the upstream `testsuite/exclude.test` /
//! `exclude-lsh.test` exclude-from file so this test pins the
//! over-deletion that was visible in CI as "Only in to: new" /
//! "Only in to/foo: down" before PR #5880 landed and remains pinned
//! after the explicit dir-only unanchored gate refactor.

use std::path::Path;

use filters::{FilterRule, FilterSet};

/// Builds the exact rule list from the upstream `exclude.test` /
/// `exclude-lsh.test` exclude-from file.
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

/// `- foo/*/` must NOT exclude files inside the sibling include
/// `+ foo/s?b/`. Before the dir-only unanchored gate landed, the
/// synthetic `foo/*/**` descendant matched `foo/sub/file1` on the
/// receiver single-path path (`set.allows`) and over-deleted.
#[test]
fn sibling_include_directory_keeps_its_children() {
    let set = FilterSet::from_rules(exclude_lsh_rules()).expect("rules compile");

    assert!(
        set.allows(Path::new("foo/sub"), true),
        "`+ foo/s?b/` must include foo/sub itself"
    );
    assert!(
        set.allows(Path::new("foo/sub/file1"), false),
        "`- foo/*/` must NOT synthesise `foo/*/**` descendants and over-exclude foo/sub/file1"
    );
    assert!(
        set.allows(Path::new("foo/sub/.filt2"), false),
        "`- foo/*/` must NOT block dotted children of foo/sub"
    );
}

/// The dir-only unanchored gate is scoped to wildcard patterns only.
/// Literal `- cache/` continues to exclude `cache/output.bin` via the
/// existing descendant matcher because the receiver single-path API
/// has no traversal context to fall back on.
#[test]
fn literal_dir_only_unanchored_pattern_keeps_descendant_semantics() {
    let set = FilterSet::from_rules([FilterRule::exclude("cache/")]).expect("rules compile");

    assert!(!set.allows(Path::new("cache"), true));
    assert!(!set.allows(Path::new("cache/output.bin"), false));
}

/// `- foo/*/` must still hide directories that match the wildcard. The
/// gate only suppresses synthetic descendants; the direct matcher on
/// the pattern itself still fires for the directory entry.
#[test]
fn dir_only_unanchored_wildcard_still_matches_directory_entry() {
    let set = FilterSet::from_rules([FilterRule::exclude("foo/*/")]).expect("rules compile");

    // `foo/other` (a directory) IS matched by `foo/*/` directly.
    assert!(!set.allows(Path::new("foo/other"), true));
    // Children of the matched directory are reported allowed by the
    // single-path API; upstream relies on the sender's traversal to
    // skip the subtree, and the receiver's deletion path does the same
    // via `allows_during_traversal`.
    assert!(set.allows(Path::new("foo/other/file"), false));
}

/// Mirrors the upstream `**/node_modules/` idiom: directory-only,
/// unanchored, leading `**` already supplies recursion. The gate must
/// suppress the otherwise-synthesised `**/node_modules/**` descendant.
#[test]
fn double_star_prefix_dir_only_pattern_suppresses_descendants() {
    let set =
        FilterSet::from_rules([FilterRule::exclude("**/node_modules/")]).expect("rules compile");

    assert!(!set.allows(Path::new("a/b/node_modules"), true));
    // The directory's contents are NOT auto-excluded by a synthetic
    // descendant; upstream emits no such rule.
    assert!(set.allows(Path::new("a/b/node_modules/package.json"), false));
}
