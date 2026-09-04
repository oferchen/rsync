//! Refusing a filter rule whose pattern is longer than the platform accepts.
//!
//! upstream: `exclude.c:1533-1538`, inside `parse_filter_str`'s token loop.
//! A rule whose pattern reaches `MAXPATHLEN` is reported and dropped:
//!
//! ```c
//! if (pat_len >= MAXPATHLEN) {
//!         rprintf(FERROR, "discarding over-long filter: %s\n",
//!                 rule_text_len(NULL, pat, (int)pat_len));
//!     free_continue:
//!         free_filter(rule); continue;
//! }
//! ```
//!
//! The `continue` is the substance: the rule is discarded and parsing carries
//! on, so an over-long rule is not a syntax error. That matters beyond the
//! message, because a pattern longer than `MAXPATHLEN` can still *match* - a
//! long run of `*` is over-long and matches everything - so keeping it would
//! change which files transfer, not merely what is printed.
//!
//! `pat_len` is the length of the pattern alone: `parse_rule_tok` has already
//! consumed the action prefix and any modifiers by this point
//! (`exclude.c:1456-1464`). The check therefore sits above both the
//! `FILTRULE_CLEAR_LIST` branch (`:1541`) and the `FILTRULE_MERGE_FILE` branch
//! (`:1550`), governing every rule kind and running before a merge directive's
//! file is ever opened.
//!
//! upstream passes `NULL` as the template here, which is not "no provenance":
//! `TEXT_FROM_FILE(NULL)` is `rule_src_in_file || false` (`exclude.c:67-69`),
//! so it falls back to whether the parser is reading a file's contents right
//! now. oc carries that same fact explicitly on
//! [`RuleSource`](crate::rule_source::RuleSource), so callers render the text
//! through `rule_text` and pass the result here - the same shape as
//! [`merge_name_overflows`](crate::merge_name_overflows).

/// Renders upstream's over-long filter refusal for `rule`.
///
/// `rule` is upstream's `rule_text_len()` rendering of the pattern - the rule
/// as written for an argument, `<rule from ...>` for one that came out of a
/// file. The argument arm is deliberately shown at full length: truncating it
/// would hide the very thing that made the rule too long.
#[must_use]
pub fn over_long_filter(rule: &str) -> String {
    format!("discarding over-long filter: {rule}")
}

/// Reports whether `pattern` exceeds what upstream will accept.
///
/// upstream compares `pat_len >= MAXPATHLEN` - `>=`, not `>`, so a pattern of
/// exactly the limit is already refused. The bound is the platform's own, via
/// [`fast_io::path_limit::max_path_len`]; see that function for why upstream's
/// 1024 fallback must not be hardcoded.
#[must_use]
pub fn is_over_long(pattern: &str) -> bool {
    pattern.len() >= fast_io::path_limit::max_path_len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rule_source::RuleSource;

    /// WHY: the wording is what upstream's testsuite greps for
    /// (`filter-merge-content-echo_test.py`), so it is part of the contract,
    /// not a message we are free to reword.
    #[test]
    fn wording_matches_upstream() {
        assert_eq!(
            over_long_filter("ARGLONG-QQQ"),
            "discarding over-long filter: ARGLONG-QQQ"
        );
    }

    /// WHY: the cell asserts `out.count('Q') > maxpath`, so the operator's own
    /// text must survive at FULL length. A helper that truncated to keep the
    /// line short would satisfy the message assertion and fail this one.
    #[test]
    fn an_argument_rule_is_shown_at_full_length() {
        let pattern = "Q".repeat(fast_io::path_limit::max_path_len() + 200);
        let rendered = over_long_filter(&RuleSource::Argument.rule_text(&pattern));

        assert_eq!(
            rendered.matches('Q').count(),
            pattern.len(),
            "the operator's own over-long rule must not be truncated",
        );
    }

    /// The non-vacuity companion: the same over-long text out of a merge file
    /// is withheld, so the pin above is testing the ARGUMENT arm specifically
    /// rather than a helper that never redacts.
    #[test]
    fn a_file_sourced_rule_is_withheld() {
        let pattern = "Q".repeat(fast_io::path_limit::max_path_len() + 200);
        let source = RuleSource::File {
            name: ".rsync-filter",
            line: 1,
        };
        let rendered = over_long_filter(&source.rule_text(&pattern));

        assert_eq!(
            rendered,
            "discarding over-long filter: <rule from .rsync-filter line 1>"
        );
        assert!(
            !rendered.contains('Q'),
            "a merge file's contents must not reach the diagnostic: {rendered}",
        );
    }

    /// upstream's comparison is `>=`, so a pattern of exactly the limit is
    /// already refused. Off-by-one here would leave one length upstream drops.
    #[test]
    fn the_bound_is_inclusive() {
        let limit = fast_io::path_limit::max_path_len();

        assert!(
            is_over_long(&"Q".repeat(limit)),
            "exactly the limit is over"
        );
        assert!(
            !is_over_long(&"Q".repeat(limit - 1)),
            "one under the limit is accepted",
        );
    }

    /// The non-vacuity companion for the bound: an ordinary rule must not be
    /// refused, or the guard would discard every filter the operator writes.
    #[test]
    fn an_ordinary_rule_is_not_over_long() {
        assert!(!is_over_long("*.tmp"));
        assert!(!is_over_long(""));
    }
}
