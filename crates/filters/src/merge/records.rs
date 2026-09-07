//! The one owner of "where does a filter-file record end?".
//!
//! Every reader that turns a filter FILE into rules has to split it first, and
//! upstream splits it in exactly one place - the character loop inside
//! `parse_filter_file` (`exclude.c:1774-1793`):
//!
//! ```c
//! if (eol_nulls? !ch : (ch == '\n' || ch == '\r')) {
//!     if (ch == '\r') {   /* CRLF is one line, not two */
//!         ...
//!         } else if (nxt != '\n' && ungetc(nxt, fp) == EOF) {
//!         ...
//!     }
//!     break;
//! }
//! ```
//!
//! A lone `\r` ends a record just as `\n` does, and `\r\n` ends exactly one.
//! Rust's [`str::lines`] breaks only on `\n` and merely strips a `\r` that sits
//! immediately before one, so a `\r`-separated filter file collapses into a
//! single record whose pattern carries the `\r` and the following rules as
//! literal bytes - it then matches nothing, and every file those rules were
//! meant to exclude transfers instead.
//!
//! That is file selection, not formatting. MEASURED against rsync 3.5.0 over a
//! source holding `a.txt` and `a.txt ` (trailing space), with a merge file
//! holding `- a.txt \r`: upstream excluded `a.txt ` and oc transferred it, both
//! at exit 0. The same divergence reproduced through `--exclude-from`, through
//! a `.rsync-filter` dir-merge, and through a daemon module's `exclude from`,
//! because each of oc's FIVE readers spelled the split for itself. Routing them
//! all through one iterator is what stops the next reader from drifting again.

/// Splits filter-file content into records the way upstream's reader does.
///
/// Records end at `\n`, at `\r`, or at `\r\n` (which ends exactly one), and the
/// terminator is not part of the record. Unterminated trailing text is a final
/// record; a trailing terminator does not manufacture an empty one.
///
/// The records are otherwise verbatim - nothing is trimmed, so trailing
/// whitespace stays part of the pattern (`exclude.c:1465`, `len = strlen(s)`).
/// Whether a record then carries a rule is [`super::filter_file_line_is_rule`].
///
/// NUL-delimited input (`--from0`, upstream's `eol_nulls`) is a different
/// split and is not this iterator's job.
#[must_use]
pub fn filter_file_records(content: &str) -> FilterFileRecords<'_> {
    FilterFileRecords {
        rest: Some(content),
    }
}

/// Iterator returned by [`filter_file_records`].
#[derive(Debug, Clone)]
pub struct FilterFileRecords<'a> {
    rest: Option<&'a str>,
}

impl<'a> Iterator for FilterFileRecords<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<&'a str> {
        let rest = self.rest?;
        let Some(end) = rest.find(['\n', '\r']) else {
            self.rest = None;
            return (!rest.is_empty()).then_some(rest);
        };
        let (record, tail) = rest.split_at(end);
        // upstream: exclude.c:1775-1791 - on `\r` it looks one character ahead
        // and swallows a following `\n`, so CRLF is one terminator.
        self.rest = Some(match tail.strip_prefix("\r\n") {
            Some(after) => after,
            None => &tail[1..],
        });
        Some(record)
    }
}

impl std::iter::FusedIterator for FilterFileRecords<'_> {}

#[cfg(test)]
mod tests {
    use super::filter_file_records;

    fn records(content: &str) -> Vec<&str> {
        filter_file_records(content).collect()
    }

    #[test]
    fn newline_separates_records() {
        assert_eq!(records("- a\n- b\n"), vec!["- a", "- b"]);
    }

    #[test]
    fn a_lone_carriage_return_ends_a_record() {
        // upstream: exclude.c:1774 - `ch == '\r'` breaks the record loop just
        // as `\n` does. MEASURED against rsync 3.5.0: a merge file holding
        // `- a.txt \r- b\r` excludes a file named `a.txt ` (trailing space).
        assert_eq!(records("- a\r- b\r"), vec!["- a", "- b"]);
        assert_eq!(records("- a\r"), vec!["- a"]);
    }

    #[test]
    fn crlf_is_one_terminator() {
        // upstream: exclude.c:1775-1791 - the `\n` after a `\r` is swallowed.
        assert_eq!(records("- a\r\n- b\r\n"), vec!["- a", "- b"]);
    }

    #[test]
    fn an_unterminated_tail_is_a_record() {
        assert_eq!(records("- a\n- b"), vec!["- a", "- b"]);
    }

    #[test]
    fn a_trailing_terminator_adds_no_empty_record() {
        assert_eq!(records("- a\n"), vec!["- a"]);
        assert_eq!(records("- a\r"), vec!["- a"]);
        assert_eq!(records("- a\r\n"), vec!["- a"]);
        assert!(records("").is_empty());
    }

    #[test]
    fn a_blank_record_is_preserved_for_the_rule_predicate() {
        // Line numbering is positional, so an empty record must still occupy
        // its slot; dropping it would misreport the line of every later rule.
        assert_eq!(records("- a\n\n- b\n"), vec!["- a", "", "- b"]);
        assert_eq!(records("- a\r\r- b\r"), vec!["- a", "", "- b"]);
    }

    #[test]
    fn trailing_whitespace_stays_in_the_record() {
        // upstream: exclude.c:1465 - the pattern length is `strlen(s)`.
        assert_eq!(records("- a \n"), vec!["- a "]);
        assert_eq!(records("- a\t\n"), vec!["- a\t"]);
    }
}
