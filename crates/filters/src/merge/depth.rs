//! The merge-file include depth cap.
//!
//! upstream: exclude.c:168 `#define MAX_MERGE_DEPTH 32`, enforced in
//! `parse_filter_file()` at exclude.c:1619-1627.
//!
//! A merge directive inside a filter file re-enters the parser on the named
//! file, so a file that merges itself - or a long chain that eventually does -
//! recurses until the stack guard page is hit. Upstream bounds the *nesting
//! depth* rather than detecting a cycle, which is the stronger of the two
//! rules: a depth cap also refuses a non-cyclic chain deeper than the cap,
//! where a "have I seen this file?" test would happily keep descending.

/// Maximum nesting depth for merge-file includes.
///
/// upstream: exclude.c:168 `#define MAX_MERGE_DEPTH 32`. Checked with `>=`
/// *before* the depth is incremented (exclude.c:1619), so 32 nested files are
/// parsed and the 33rd entry is refused.
pub const MAX_MERGE_DEPTH: usize = 32;

/// Renders upstream's depth-limit diagnostic.
///
/// upstream: exclude.c:1620-1622
/// ```text
/// rprintf(FERROR, "[%s] merge-file include depth limit (%d) exceeded at %s\n",
///         who_am_i(), MAX_MERGE_DEPTH, rule_text(template, fname));
/// ```
///
/// `role` is upstream's `who_am_i()` and `rule` its `rule_text()` rendering of
/// the offending merge directive - the rule as written, not the resolved path,
/// so the operator sees what they typed.
#[must_use]
pub fn depth_limit_exceeded(role: &str, rule: &str) -> String {
    format!("[{role}] merge-file include depth limit ({MAX_MERGE_DEPTH}) exceeded at {rule}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WHY: the cap is a wire-visible constant an operator can hit; it must be
    /// upstream's value, not a rounded-off one. A different number changes
    /// which filter sets parse.
    #[test]
    fn cap_matches_upstream() {
        assert_eq!(MAX_MERGE_DEPTH, 32, "upstream: exclude.c:168");
    }

    /// WHY: the testsuite cell `filter-merge-recursion` greps for the exact
    /// substring `merge-file include depth limit`, and an operator comparing
    /// oc against rsync should see the same line. Pin the whole rendering, not
    /// just the substring, so a reworded prefix cannot drift silently.
    #[test]
    fn diagnostic_matches_upstream_wording() {
        assert_eq!(
            depth_limit_exceeded("sender", ". .merge"),
            "[sender] merge-file include depth limit (32) exceeded at . .merge"
        );
    }
}
