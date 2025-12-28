use super::{
    modifiers::{parse_merge_modifiers, split_short_merge_modifiers},
    types::{FilterParseError, ParsedFilterDirective},
};
use std::path::PathBuf;

pub(super) fn parse_merge_directive(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    const MERGE_PREFIX: &str = "merge";

    if text.len() < MERGE_PREFIX.len() {
        return Ok(None);
    }

    let (prefix, rest) = text.split_at(MERGE_PREFIX.len());
    if !prefix.eq_ignore_ascii_case(MERGE_PREFIX) {
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

    let path_text = remainder.trim_end();
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

pub(super) fn parse_short_merge_directive_line(
    text: &str,
) -> Result<Option<ParsedFilterDirective>, FilterParseError> {
    let mut chars = text.chars();
    let first = match chars.next() {
        Some(first) => first,
        None => return Ok(None),
    };

    let allow_extended = match first {
        '.' => false,
        ':' => true,
        _ => return Ok(None),
    };

    let remainder = chars.as_str();
    let (modifiers, rest) = split_short_merge_modifiers(remainder, allow_extended);
    let (options, assume_cvsignore) = parse_merge_modifiers(modifiers, text, allow_extended)?;

    let pattern = rest.trim();
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
        return Ok(Some(ParsedFilterDirective::Merge {
            path: PathBuf::from(pattern),
            options: Some(options),
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

    // ==================== parse_merge_directive tests ====================

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

    #[test]
    fn parse_merge_directive_case_insensitive() {
        let result = parse_merge_directive("MERGE .rsync-filter");
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

    // ==================== parse_short_merge_directive_line tests ====================

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
            ParsedFilterDirective::Merge { path, options } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
                assert!(options.is_some());
            }
            _ => panic!("expected Merge directive"),
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

    #[test]
    fn parse_short_merge_trims_pattern() {
        let result = parse_short_merge_directive_line(".   .rsync-filter   ");
        assert!(result.is_ok());
        let directive = result.unwrap().unwrap();
        match directive {
            ParsedFilterDirective::Merge { path, .. } => {
                assert_eq!(path, PathBuf::from(".rsync-filter"));
            }
            _ => panic!("expected Merge directive"),
        }
    }
}
