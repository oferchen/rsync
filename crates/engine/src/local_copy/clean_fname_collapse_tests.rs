use super::lexically_normalize;
use std::path::{Path, PathBuf};
use test_support::COLLAPSE_CASES;

/// Every upstream `clean_fname(name, CFN_COLLAPSE_DOT_DOT_DIRS)` case must
/// collapse identically here.
///
/// `lexically_normalize` is the alt-basis resolver for `--link-dest`,
/// `--copy-dest` and `--compare-dest`: it decides which directory a basis file
/// is read from, and its result is what any containment check downstream sees.
/// A `..` that fails to consume the component before it therefore does not
/// merely produce an ugly path - it resolves the basis somewhere other than
/// where the collapsed name points, which is the traversal shape upstream's
/// off-by-one had. Pinning the upstream table here keeps that rule honest.
///
/// The table is shared (`test_support::COLLAPSE_CASES`) so a new edge case is
/// one row and is checked against every oc-rsync copy of this rule at once.
// upstream: util1.c clean_fname() CFN_COLLAPSE_DOT_DOT_DIRS; t_clean_fname.c
#[test]
fn upstream_collapse_cases_consume_the_preceding_component() {
    for (input, expected) in COLLAPSE_CASES {
        assert_eq!(
            lexically_normalize(Path::new(input)),
            PathBuf::from(expected),
            "clean_fname collapse case {input:?}"
        );
    }
}

/// A `..` with nothing before it is kept verbatim, and `/..` stays at the root.
///
/// Upstream's `clean_fname` documents the collapse as applying "except at the
/// start", and its `s == name && anchored` guard drops a root-relative `..`
/// rather than climbing above `/`. Both arms matter here: keeping the leading
/// `..` is what lets the caller's containment check see the escape and reject
/// it, and refusing to climb past the root is what stops an absolute basis from
/// walking out of the filesystem root.
// upstream: util1.c clean_fname() - "collapse '..' elements (except at the start)"
#[test]
fn leading_dot_dot_is_preserved_and_root_dot_dot_stays_at_root() {
    assert_eq!(
        lexically_normalize(Path::new("../a")),
        PathBuf::from("../a")
    );
    assert_eq!(
        lexically_normalize(Path::new("../../a")),
        PathBuf::from("../../a")
    );
    assert_eq!(lexically_normalize(Path::new("/..")), PathBuf::from("/"));
    assert_eq!(lexically_normalize(Path::new("/../a")), PathBuf::from("/a"));
}
