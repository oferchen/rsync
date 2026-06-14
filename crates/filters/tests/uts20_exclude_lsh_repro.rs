//! UTS-20 followup repro: bare interior `**` must match across path
//! separators, matching upstream rsync's wildmatch semantics.
//!
//! Upstream command (from the `exclude-lsh` testsuite case):
//! `rsync -av -f '+ foo**too' -f '- *' from/ chk/`
//!
//! Upstream `lib/wildmatch.c:dowild()` treats `**` as a recursive wildcard
//! that matches any sequence of characters including `/`, regardless of
//! the characters surrounding it. Without normalisation, globset's
//! `literal_separator(true)` interprets bare `**` as two independent `*`
//! wildcards (each of which stops at `/`), so `foo**too` would only match
//! within a single path segment.
//!
//! Regression assertions: a pattern of `foo**too` must include
//! `bar/down/to/foo/too` (cross-segment), `foo/too` (basename), AND
//! `fooxytoo` (in-segment), to retain upstream parity.

use std::path::Path;

use filters::{FilterRule, FilterSet};

/// `+ foo**too` followed by `- *` must include any path whose components
/// start with `foo` and end with `too`, including the directory
/// `bar/down/to/foo/too` from the `exclude-lsh` testsuite.
#[test]
fn uts20_recursive_glob_matches_cross_segment_directory() {
    let rules = [FilterRule::include("foo**too"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    // Directory form: `bar/down/to/foo/too/` as a directory must be allowed.
    assert!(
        set.allows(Path::new("bar/down/to/foo/too"), true),
        "directory bar/down/to/foo/too must be included by `+ foo**too`"
    );
    // Basename form: minimal cross-segment match.
    assert!(set.allows(Path::new("foo/too"), true));
    // In-segment form: `**` must still degenerate to `*` semantics inside
    // a single path component.
    assert!(set.allows(Path::new("fooxytoo"), false));
}

/// Negative companion: paths that neither begin with `foo` nor end with
/// `too` are NOT included by `+ foo**too`, so the trailing `- *` excludes
/// them. Guards against an over-broad normalisation.
#[test]
fn uts20_recursive_glob_does_not_overmatch() {
    let rules = [FilterRule::include("foo**too"), FilterRule::exclude("*")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("bar"), true));
    assert!(!set.allows(Path::new("foo/other"), false));
    assert!(!set.allows(Path::new("prefix/too"), false));
}

/// Regression guard: existing slash-bounded `**` patterns must continue
/// to behave identically after normalisation. `**/*.log` still excludes
/// every `.log` file at any depth.
#[test]
fn uts20_existing_double_star_prefix_unchanged() {
    let rules = [FilterRule::exclude("**/*.log"), FilterRule::include("**")];
    let set = FilterSet::from_rules(rules).unwrap();

    assert!(!set.allows(Path::new("debug.log"), false));
    assert!(!set.allows(Path::new("a/b/c/debug.log"), false));
    assert!(set.allows(Path::new("debug.txt"), false));
}
