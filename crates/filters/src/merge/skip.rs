//! The one owner of "does this filter-file record carry a rule?".
//!
//! Every reader that turns a filter FILE into rules has to answer this, and
//! upstream answers it in exactly one place - the tail of `parse_filter_file`'s
//! read loop (`exclude.c:1806`):
//!
//! ```c
//! /* Skip an empty token and (when line parsing) comments. */
//! if (*line && (word_split || (*line != ';' && *line != '#')))
//!     parse_filter_str(listp, line, template, xflags);
//! ```
//!
//! Two properties of that line are load-bearing and were repeatedly lost when
//! each reader spelled the test for itself:
//!
//! 1. It tests `*line` and `line[0]` - the FIRST BYTE - with no trimming. A
//!    whitespace-only record is not empty, and `  # x` is not a comment.
//! 2. `word_split ||` short-circuits, so in a `w`/`C` merge file `#` and `;`
//!    are ordinary pattern bytes.
//!
//! Both matter to file selection, not to formatting: whatever survives this
//! test becomes a pattern matched literally against names. oc had four readers
//! spelling it four ways, and two of them trimmed, which changed which files
//! transferred at exit 0. Routing them all through one predicate is what stops
//! the next reader from drifting again.

/// Reports whether a filter-file record should be handed to the rule parser.
///
/// `comments_recognised` is upstream's `!word_split`: a line-parsed filter file
/// treats a leading `;`/`#` as a comment, and a word-split one (`:w`, `:C`,
/// `merge,w`) does not.
///
/// The record must already have had its delimiter removed - upstream's reader
/// stops before the `\n`/`\r`/NUL - and is otherwise passed verbatim.
#[must_use]
pub fn filter_file_line_is_rule(line: &str, comments_recognised: bool) -> bool {
    if line.is_empty() {
        return false;
    }
    if comments_recognised && (line.starts_with('#') || line.starts_with(';')) {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::filter_file_line_is_rule;

    #[test]
    fn an_empty_record_carries_no_rule() {
        assert!(!filter_file_line_is_rule("", true));
        assert!(!filter_file_line_is_rule("", false));
    }

    #[test]
    fn a_whitespace_only_record_is_a_rule() {
        // upstream tests `*line`, so `   ` is not empty and reaches the parser
        // (where it then fails as `Unknown filter rule`). MEASURED against
        // rsync 3.5.0: a `.rsync-filter` whose first line is `   ` exits 1.
        assert!(filter_file_line_is_rule("   ", true));
        assert!(filter_file_line_is_rule("\t", true));
    }

    #[test]
    fn a_comment_marker_counts_only_in_column_zero() {
        assert!(!filter_file_line_is_rule("# x", true));
        assert!(!filter_file_line_is_rule("; x", true));
        // MEASURED against rsync 3.5.0: an `--exclude-from` file holding only
        // `  #a` excludes a file literally named `  #a`.
        assert!(filter_file_line_is_rule("  # x", true));
        assert!(filter_file_line_is_rule("  ; x", true));
    }

    #[test]
    fn word_split_files_have_no_comments() {
        // upstream: `word_split ||` short-circuits the comment test entirely.
        assert!(filter_file_line_is_rule("# x", false));
        assert!(filter_file_line_is_rule("; x", false));
    }

    #[test]
    fn an_ordinary_rule_is_a_rule() {
        assert!(filter_file_line_is_rule("- *.log", true));
        assert!(filter_file_line_is_rule("- a ", true));
    }
}
