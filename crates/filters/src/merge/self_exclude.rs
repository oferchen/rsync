//! The extra exclude a `:e` / `.e` dir-merge directive synthesises for the
//! merge file's own name.
//!
//! upstream: `exclude.c:1558-1571`, inside `parse_filter_str`'s
//! `FILTRULE_MERGE_FILE` branch:
//!
//! ```c
//! if (new_rflags & FILTRULE_EXCLUDE_SELF) {
//!         excl_self = new0(filter_rule);
//!         excl_self->rflags = rule->rflags & FILTRULE_FROM_FILE;
//!         /* Find the beginning of the basename and add an exclude for it. */
//!         for (name = pat + pat_len; name > pat && name[-1] != '/'; name--) {}
//!         add_rule(listp, name, (pat + pat_len) - name, excl_self, 0);
//!         rule->rflags &= ~FILTRULE_EXCLUDE_SELF;
//! }
//! ```
//!
//! Three properties of that block decide where a caller must put the rule, and
//! all three are easy to lose by building it somewhere more convenient:
//!
//! - it runs at REGISTRATION - when the directive is parsed - so the exclude
//!   exists whether or not a file by that name is ever found in any directory;
//! - it goes into `listp`, the ENCLOSING list, and `FILTRULE_EXCLUDE_SELF` is
//!   then cleared, so the rule is added exactly once rather than once per
//!   directory that happens to contain the file;
//! - it precedes the merge rule's own `add_rule` at `:1590`, and filter lists
//!   are first-match-wins, so the order is part of the behaviour.
//!
//! The provenance is the fourth: the synthesised rule inherits only
//! `FILTRULE_FROM_FILE` from its parent. upstream's own comment above that line
//! records why - built by hand with no template, the rule looked
//! argument-origin once parsing finished and the match trace echoed a merge
//! file's contents at `-vv`. Callers therefore attach the provenance of the
//! directive they are expanding, not of the rule they are creating.

/// Returns the basename upstream excludes for a `:e` merge directive.
///
/// upstream scans back from the end of the pattern to the byte after the last
/// `/` (`exclude.c:1568-1569`), so this is a byte split, not a path parse: a
/// pattern ending in `/` yields the empty name and `..` yields `..`, both of
/// which `Path::file_name` would instead report as absent. Mirroring the scan
/// keeps those inputs behaving as upstream does rather than as a path API
/// would prefer.
#[must_use]
pub fn merge_self_exclude_name(pattern: &str) -> &str {
    match pattern.rfind('/') {
        Some(slash) => &pattern[slash + 1..],
        None => pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordinary case: a bare merge-file name is its own basename.
    #[test]
    fn a_bare_name_is_unchanged() {
        assert_eq!(merge_self_exclude_name(".rsync-filter"), ".rsync-filter");
    }

    /// WHY: upstream excludes the NAME, not the path it was written with, so
    /// `:e sub/name` hides `name` in each directory - not `sub/name`.
    #[test]
    fn a_qualified_name_keeps_only_the_last_segment() {
        assert_eq!(merge_self_exclude_name("sub/name"), "name");
        assert_eq!(merge_self_exclude_name("a/b/c/name"), "name");
        assert_eq!(merge_self_exclude_name("/leading"), "leading");
    }

    /// WHY: the scan is over bytes, so a trailing slash leaves NOTHING after
    /// the last one. `Path::file_name` reports `sub` here, which would exclude
    /// a directory upstream never names.
    #[test]
    fn a_trailing_slash_yields_the_empty_name() {
        assert_eq!(merge_self_exclude_name("sub/"), "");
    }

    /// The other place a path API and upstream's byte scan disagree:
    /// `Path::file_name` returns `None` for `..`, upstream returns `..`.
    #[test]
    fn dot_dot_is_its_own_basename() {
        assert_eq!(merge_self_exclude_name(".."), "..");
        assert_eq!(merge_self_exclude_name("sub/.."), "..");
    }

    /// The glob in upstream's `filter-merge-content-echo` cell: the literal
    /// text is what gets excluded, brackets and all, and it is that literal
    /// which must never reach a diagnostic when it came out of a file.
    #[test]
    fn a_glob_name_is_carried_through_verbatim() {
        assert_eq!(merge_self_exclude_name("excl-self-[x]9"), "excl-self-[x]9");
    }
}
