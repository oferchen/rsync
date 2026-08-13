//! Lexical `..` collapse, mirroring upstream `clean_fname()` with
//! `CFN_COLLAPSE_DOT_DOT_DIRS`.
//!
//! Upstream cleans a merge-file name before opening it, so `merge a/b/../test`
//! reads `a/test` even when `a/b` does not exist. That is a lexical rewrite,
//! not a filesystem resolution: it never touches the disk and never follows a
//! symlink, which is what makes it well defined for a path whose intermediate
//! directories are absent.
//!
//! rsync 3.5.0 fixed an off-by-one that left this collapse dead for every
//! multi-component and absolute path, so pre-3.5.0 releases open the uncleaned
//! name instead.
//!
//! # Upstream References
//!
//! - `util1.c` - `clean_fname()`, the `CFN_COLLAPSE_DOT_DOT_DIRS` branch.
//! - `exclude.c` - `parse_merge_name()` cleans the merge-file name (and again
//!   after joining it onto the filter dir) before the file is read.

use std::path::{Component, Path, PathBuf};

/// Collapses `.` and `..` components in `path` purely lexically.
///
/// Mirrors upstream `clean_fname(name, CFN_COLLAPSE_DOT_DOT_DIRS)`:
///
/// - a `..` consumes the component before it;
/// - a `..` with nothing left to consume is kept, so a relative path that
///   climbs above its anchor still says so;
/// - a `..` directly under the root stays at the root, matching upstream's
///   `s == name && anchored` guard;
/// - `.` components and duplicate separators are dropped;
/// - an empty result becomes `"."`.
///
/// The rewrite is lexical, so it is defined for paths whose intermediate
/// directories do not exist - and, like upstream, it does not agree with the
/// kernel when a consumed component is a symlink.
///
/// # Upstream Reference
///
/// - `util1.c` - `clean_fname()` with `CFN_COLLAPSE_DOT_DOT_DIRS`.
#[must_use]
pub fn collapse_dot_dot_dirs(path: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // upstream: util1.c clean_fname() - `s == name && anchored`
                // drops a root-relative `..` instead of climbing past `/`.
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(component),
            },
            other => out.push(other),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::collapse_dot_dot_dirs;
    use std::path::{Path, PathBuf};

    /// Every upstream `clean_fname(name, CFN_COLLAPSE_DOT_DOT_DIRS)` case must
    /// collapse identically here.
    ///
    /// This helper decides which file a `merge`/`dir-merge` rule opens, so a
    /// `..` that fails to consume the component before it does not just render
    /// oddly - it reads a different file, or none. The table is upstream's own
    /// `t_clean_fname.c` `cases[]`, shared via `test_support` so a new edge case
    /// is one row and reaches every oc-rsync copy of this rule at once.
    // upstream: util1.c clean_fname() CFN_COLLAPSE_DOT_DOT_DIRS; t_clean_fname.c
    #[test]
    fn upstream_collapse_cases_consume_the_preceding_component() {
        for (input, expected) in test_support::COLLAPSE_CASES {
            assert_eq!(
                collapse_dot_dot_dirs(Path::new(input)),
                PathBuf::from(expected),
                "clean_fname collapse case {input:?}"
            );
        }
    }

    /// The boundary arms `clean_fname` documents, measured against a real
    /// rsync 3.5.0 build rather than derived from reading: a leading `..` with
    /// nothing to consume survives, `/..` stays at the root, and a path that
    /// cancels out becomes `"."`.
    // upstream: util1.c clean_fname() - "collapse '..' elements (except at the start)"
    #[test]
    fn boundary_arms_match_upstream() {
        for (input, expected) in [
            ("../a", "../a"),
            ("../../a", "../../a"),
            ("/..", "/"),
            ("/../a", "/a"),
            ("..", ".."),
            ("a/..", "."),
            ("mod/../../outside", "../outside"),
            ("a/b/../../c", "c"),
            ("./a/../b", "b"),
        ] {
            assert_eq!(
                collapse_dot_dot_dirs(Path::new(input)),
                PathBuf::from(expected),
                "clean_fname boundary case {input:?}"
            );
        }
    }
}
