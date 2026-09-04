//! Diagnostic for a merge-file name too long to compose.
//!
//! A per-directory merge rule names a file that is looked up again in every
//! directory the scan enters, so the name that reaches the filesystem is
//! `<scanned directory>/<rule pattern>`. The rule can be short enough to parse
//! and still overflow once that prefix is prepended - which is exactly the case
//! upstream refuses here, before any open.
//!
//! The refusal has to route the pattern through the provenance funnel: a
//! deferred `:` rule is normally read out of a merge file, so echoing its text
//! would leak that file's contents at *no* verbosity, from an operator's plain
//! `-r -F`.

/// Renders upstream's merge-name overflow diagnostic.
///
/// upstream: exclude.c:727 and exclude.c:741, the two `parse_merge_name`
/// refusals:
///
/// ```text
/// rprintf(FERROR, "merge-file name overflows: %s\n",
///         rule_text(template, merge_file));
/// ```
///
/// `rule` is upstream's `rule_text()` rendering of the merge directive's
/// pattern - the rule as written for an argument, `<rule from ...>` for one
/// that came out of a file. Pass the pattern, never the resolved path: the
/// resolved path is what overflowed, but it embeds the pattern verbatim and
/// would defeat the redaction.
#[must_use]
pub fn merge_name_overflows(rule: &str) -> String {
    format!("merge-file name overflows: {rule}")
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
            merge_name_overflows("sub/name"),
            "merge-file name overflows: sub/name"
        );
    }

    /// WHY: this is the leak the diagnostic exists to avoid - a deferred
    /// per-dir merge whose pattern came out of a `.rsync-filter`. Without the
    /// funnel the whole name is echoed with no verbosity requested at all.
    #[test]
    fn a_file_sourced_pattern_is_withheld() {
        let rendered =
            merge_name_overflows(&RuleSource::FileReadEarlier.rule_text("sub/DEFERRED-SECRET"));
        assert_eq!(
            rendered,
            "merge-file name overflows: <rule from a file read earlier>"
        );
        assert!(!rendered.contains("DEFERRED-SECRET"));
    }

    /// WHY: the non-vacuity companion. A blanket-redaction bug would satisfy
    /// the test above; upstream keeps an argument rule's own text because it is
    /// the operator's and hiding it only makes typos harder to fix
    /// (exclude.c:90-95).
    #[test]
    fn an_argument_pattern_is_shown_verbatim() {
        assert_eq!(
            merge_name_overflows(&RuleSource::Argument.rule_text("sub/TYPO")),
            "merge-file name overflows: sub/TYPO"
        );
    }
}
