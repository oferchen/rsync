use super::{
    modifiers::{parse_merge_modifiers, split_short_merge_modifiers},
    types::{FilterParseError, ParsedFilterDirective},
};
use std::path::PathBuf;

/// Parses a `merge` directive of the form `merge[,modifiers] PATH`.
///
/// Returns `Ok(None)` when the input does not start with the `merge` keyword.
/// Rejects `merge -` (stdin merges are not allowed inside `.rsync-filter`
/// files) and missing-path inputs unless the `,C` modifier supplies the
/// implicit `.cvsignore` default. `options` is `None` only when no modifiers
/// were specified, preserving inheritance of the caller's parser settings.
pub(super) fn parse_merge_directive(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    const MERGE_PREFIX: &str = "merge";

    if text.len() < MERGE_PREFIX.len() {
        return Ok(None);
    }

    // upstream: exclude.c:1310 `RULE_STRCMP(s, "merge")` under `case 'm':`, and
    // `rule_strcmp` is `strncmp` (:1218) - `Merge` reaches `default:` and is
    // reported as an unknown rule, not parsed as a merge directive.
    let (prefix, rest) = text.split_at(MERGE_PREFIX.len());
    if prefix != MERGE_PREFIX {
        return Ok(None);
    }

    let mut remainder = rest.trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    let mut modifiers = "";
    if let Some(next) = remainder.strip_prefix(',') {
        let mut split = next.splitn(2, |ch: char| ch.is_ascii_whitespace() || ch == '_');
        modifiers = split.next().unwrap_or("");
        remainder = split
            .next()
            .unwrap_or("")
            .trim_start_matches(|ch: char| ch == '_' || ch.is_ascii_whitespace());
    }

    let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, text, false)?;

    if remainder == "-" {
        return Err(FilterParseError::new(
            "merge from standard input is not supported in .rsync-filter files",
        ));
    }

    // The merge PATH is the rest of the rule verbatim: upstream consumes one
    // separator after the keyword (exclude.c:1444-1445) and then takes
    // `len = strlen(s)` (exclude.c:1465); `parse_merge_name` (exclude.c:696-752)
    // only runs `clean_fname` (:734), which never touches whitespace. MEASURED
    // against rsync 3.5.0 with a `.rsync-filter` holding `merge inner ` and a
    // filter file literally named `inner `: upstream opens it and excludes `b`;
    // oc trimmed the name and failed the transfer with exit 24.
    let path_text = remainder;
    let path_text = if path_text.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else {
            return Err(FilterParseError::new(
                "merge directive requires a file path",
            ));
        }
    } else {
        path_text
    };

    let options = if modifiers.is_empty() && !assume_cvsignore {
        None
    } else {
        Some(options)
    };

    Ok(Some(ParsedFilterDirective::Merge {
        path: PathBuf::from(path_text),
        options,
    }))
}

/// Parses the short-form merge prefixes `.` (plain merge) and `:` (dir-merge).
///
/// Returns `Ok(None)` when the input begins with any other character. The
/// `:` form enables extended modifiers (`n`, `e`, `w`, `s`, `r`, `/`) and
/// always returns explicit `options`. The `.` form mirrors the long-form
/// `merge` semantics, returning `None` options when no modifiers were given.
pub(super) fn parse_short_merge_directive_line(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return Ok(None);
    };

    let allow_extended = match first {
        '.' => false,
        ':' => true,
        _ => return Ok(None),
    };

    let remainder = chars.as_str();
    let (modifiers, rest) = split_short_merge_modifiers(remainder, allow_extended);
    let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, text, allow_extended)?;

    // `split_short_merge_modifiers` already consumed the ONE separator that ends
    // the modifier run (upstream exclude.c:1444-1445, `if (*s) s++`), and the
    // merge FILENAME is then the rest of the rule verbatim (`len = strlen(s)`,
    // exclude.c:1465). `parse_merge_name` (exclude.c:696-752) only runs
    // `clean_fname` over it (:734), which collapses slashes and `..`, never
    // whitespace. So a trailing space is part of the merge file's NAME.
    //
    // Trimming here changed WHICH FILES TRANSFER. MEASURED against rsync 3.5.0
    // with a `.rsync-filter` holding `: inner ` over a source containing `a`,
    // `b` and a filter file literally named `inner ` that reads `- b`:
    // upstream copies `a` and `inner `; oc trimmed the name, found nothing,
    // and copied `b` too. `. inner ` failed the transfer outright (exit 24).
    let pattern = rest;
    let pattern = if pattern.is_empty() {
        if assume_cvsignore {
            ".cvsignore"
        } else if allow_extended {
            return Err(FilterParseError::new(format!(
                "dir-merge directive '{text}' is missing a file name"
            )));
        } else {
            return Err(FilterParseError::new(format!(
                "merge directive '{text}' is missing a file path"
            )));
        }
    } else {
        pattern
    };

    if allow_extended {
        // upstream: exclude.c:1419-1428 - ':' short form is a per-directory
        // merge that registers a filename to look up in each subdirectory,
        // not an eager merge of the parent file's adjacent rule file.
        return Ok(Some(ParsedFilterDirective::DirMerge {
            pattern: PathBuf::from(pattern),
            options,
        }));
    }

    let options = if modifiers.is_empty() && !assume_cvsignore {
        None
    } else {
        Some(options)
    };

    Ok(Some(ParsedFilterDirective::Merge {
        path: PathBuf::from(pattern),
        options,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_merge_directive_returns_none_for_non_merge() {
        let result = parse_merge_directive("include *.txt");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn parse_merge_directive_returns_none_for_short_text() {
        let result = parse_merge_directive("merg");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn parse_merge_directive_parses_simple_merge() {
        let result = parse_merge_directive("merge .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { path, options } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
                assert!(options.is_none());
            }
            _ => panic!("expected Merge directive"),
        }
    }

    /// upstream: exclude.c:1310 - `RULE_STRCMP(s, "merge")` is `strncmp`
    /// (:1218), so the keyword is lower case only. MEASURED against rsync
    /// 3.5.0: `--filter='Merge f'` reports `Unknown filter rule` and exits 1.
    #[test]
    fn parse_merge_keyword_is_case_sensitive() {
        assert!(
            parse_merge_directive("MERGE .rsync-filter")
                .expect("uppercase is not a parse error, just not a merge")
                .is_none()
        );
    }

    /// Non-vacuity companion for `parse_merge_keyword_is_case_sensitive`:
    /// without it the case test would also pass if the parser recognised no
    /// spelling at all.
    #[test]
    fn parse_merge_lower_case_keyword_still_parses() {
        let directive = parse_merge_directive("merge .rsync-filter")
            .expect("parse")
            .expect("merge directive");
        match directive {
            ParsedFilterDirective::Merge { path, .. } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_merge_directive_with_underscore() {
        let result = parse_merge_directive("merge_.rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { path, .. } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_merge_directive_error_on_stdin() {
        let result = parse_merge_directive("merge -");
        assert!(result.is_err());
    }

    #[test]
    fn parse_merge_directive_error_missing_path() {
        let result = parse_merge_directive("merge ");
        assert!(result.is_err());
    }

    #[test]
    fn parse_short_merge_returns_none_for_empty() {
        let result = parse_short_merge_directive_line("");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn parse_short_merge_returns_none_for_non_merge_prefix() {
        let result = parse_short_merge_directive_line("+ *.txt");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn parse_short_merge_dot_prefix() {
        let result = parse_short_merge_directive_line(". .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { path, options } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
                assert!(options.is_none());
            }
            _ => panic!("expected Merge directive"),
        }
    }

    #[test]
    fn parse_short_merge_colon_prefix() {
        let result = parse_short_merge_directive_line(": .rsync-filter");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::DirMerge { pattern, .. } => {
                assert_eq!(pattern, PathBuf::from(".rsync-filter"));
            }
            _ => panic!("expected DirMerge directive"),
        }
    }

    #[test]
    fn parse_short_merge_colon_error_missing_filename() {
        let result = parse_short_merge_directive_line(":");
        assert!(result.is_err());
    }

    #[test]
    fn parse_short_merge_dot_error_missing_path() {
        let result = parse_short_merge_directive_line(".");
        assert!(result.is_err());
    }

    /// upstream: exclude.c:1444-1445 consumes ONE separator after the `.`, and
    /// exclude.c:1465 then takes `len = strlen(s)`, so every remaining byte -
    /// two more leading spaces and three trailing ones - is part of the merge
    /// file's NAME. `parse_merge_name` (exclude.c:696-752) only runs
    /// `clean_fname` (:734), which never strips whitespace.
    ///
    /// This test previously asserted the trimmed name. MEASURED against rsync
    /// 3.5.0, `--filter=':  '` transfers a file literally named `  ` and
    /// `--filter='.  '` fails to open the merge file named ` `; both prove the
    /// whitespace survives.
    #[test]
    fn parse_short_merge_keeps_the_whitespace_around_its_name() {
        let result = parse_short_merge_directive_line(".   .rsync-filter   ");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { path, .. } => {
                assert_eq!(path, PathBuf::from("  .rsync-filter   "));
            }
            _ => panic!("expected Merge directive"),
        }
    }
}
